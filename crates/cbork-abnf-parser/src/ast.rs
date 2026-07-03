// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::{
    collections::{BTreeSet, HashMap},
    convert::TryFrom,
    fmt,
    ops::Range,
};

use pest::error::Error;

use crate::abnf;

/// A byte and line/column span in the original ABNF source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSpan {
    /// The byte range in the original source.
    range: Range<usize>,
    /// The one-based start line.
    start_line: usize,
    /// The one-based start column.
    start_column: usize,
    /// The one-based end line.
    end_line: usize,
    /// The one-based end column.
    end_column: usize,
}

impl SourceSpan {
    /// Construct a new source span.
    #[must_use]
    pub(crate) fn new(
        range: Range<usize>,
        start_line: usize,
        start_column: usize,
        end_line: usize,
        end_column: usize,
    ) -> Self {
        Self {
            range,
            start_line,
            start_column,
            end_line,
            end_column,
        }
    }

    /// Return the byte range covered by the span.
    #[must_use]
    pub fn range(&self) -> &Range<usize> {
        &self.range
    }

    /// Return the one-based starting line number.
    #[must_use]
    pub fn start_line(&self) -> usize {
        self.start_line
    }

    /// Return the one-based starting column number.
    #[must_use]
    pub fn start_column(&self) -> usize {
        self.start_column
    }

    /// Return the one-based ending line number.
    #[must_use]
    pub fn end_line(&self) -> usize {
        self.end_line
    }

    /// Return the one-based ending column number.
    #[must_use]
    pub fn end_column(&self) -> usize {
        self.end_column
    }
}

/// An owned ABNF document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbnfDocument {
    /// The original ABNF source text.
    source: String,
    /// The parsed rules in source order.
    rules: Vec<AbnfRule>,
}

impl AbnfDocument {
    /// Construct a new document from source text and parsed rules.
    #[must_use]
    pub(crate) fn new(
        source: String,
        rules: Vec<AbnfRule>,
    ) -> Self {
        Self { source, rules }
    }

    /// Return the original source text.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Return the parsed rules in source order.
    #[must_use]
    pub fn rules(&self) -> &[AbnfRule] {
        &self.rules
    }

    /// Validate a byte string against the document's first rule.
    ///
    /// # Errors
    ///
    /// Returns an error if the document has no rules, contains an unsupported
    /// ABNF construct, or the input does not match the start rule.
    pub fn validate_bytes(
        &self,
        input: impl AsRef<[u8]>,
    ) -> Result<(), AbnfValidationError> {
        self.validate_input(input.as_ref())
    }

    /// Validate a text string against the document's first rule.
    ///
    /// # Errors
    ///
    /// Returns an error if the document has no rules, contains an unsupported
    /// ABNF construct, or the input does not match the start rule.
    pub fn validate_text(
        &self,
        input: impl AsRef<str>,
    ) -> Result<(), AbnfValidationError> {
        self.validate_input(input.as_ref().as_bytes())
    }

    /// Validate a byte slice against the start rule.
    fn validate_input(
        &self,
        input: &[u8],
    ) -> Result<(), AbnfValidationError> {
        let Some(start_rule) = self.rules.first() else {
            return Err(AbnfValidationError::EmptyDocument);
        };

        let mut validator = AbnfValidator::new(self);
        let matches = validator.match_rule(start_rule.name().as_str(), 0, input)?;
        if matches.contains(&input.len()) {
            Ok(())
        } else {
            let offset = matches.iter().copied().max().unwrap_or(0);
            Err(AbnfValidationError::Mismatch {
                offset,
                expected: start_rule.name().as_str().to_owned(),
            })
        }
    }
}

impl fmt::Display for AbnfDocument {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        write!(
            f,
            "source_len={} rules={}",
            self.source.len(),
            self.rules.len()
        )
    }
}

/// A top-level ABNF rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbnfRule {
    /// The rule name.
    name: Rulename,
    /// The rule definition operator.
    operator: DefinitionOperator,
    /// The rule expression.
    expression: Alternation,
    /// The source span of the full rule.
    span: SourceSpan,
}

impl AbnfRule {
    /// Construct a new ABNF rule.
    #[must_use]
    pub(crate) fn new(
        name: Rulename,
        operator: DefinitionOperator,
        expression: Alternation,
        span: SourceSpan,
    ) -> Self {
        Self {
            name,
            operator,
            expression,
            span,
        }
    }

    /// Return the rule name.
    #[must_use]
    pub fn name(&self) -> &Rulename {
        &self.name
    }

    /// Return the rule definition operator.
    #[must_use]
    pub fn operator(&self) -> DefinitionOperator {
        self.operator
    }

    /// Return the rule expression.
    #[must_use]
    pub fn expression(&self) -> &Alternation {
        &self.expression
    }

    /// Return the source span for the full rule.
    #[must_use]
    pub fn span(&self) -> &SourceSpan {
        &self.span
    }
}

/// The rule definition operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefinitionOperator {
    /// `=`
    Assign,
    /// `=/`
    Incremental,
}

/// A rule name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rulename {
    /// The rule name text.
    name: String,
    /// The source span of the rule name.
    span: SourceSpan,
}

impl Rulename {
    /// Construct a new rule name.
    #[must_use]
    pub(crate) fn new(
        name: String,
        span: SourceSpan,
    ) -> Self {
        Self { name, span }
    }

    /// Return the rule name text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.name
    }

    /// Return the source span for the rule name.
    #[must_use]
    pub fn span(&self) -> &SourceSpan {
        &self.span
    }
}

/// An alternation, separated by `/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alternation {
    /// The concatenation branches in source order.
    concatenations: Vec<Concatenation>,
    /// The source span of the alternation.
    span: SourceSpan,
}

impl Alternation {
    /// Construct a new alternation.
    #[must_use]
    pub(crate) fn new(
        concatenations: Vec<Concatenation>,
        span: SourceSpan,
    ) -> Self {
        Self {
            concatenations,
            span,
        }
    }

    /// Return the concatenation branches in source order.
    #[must_use]
    pub fn concatenations(&self) -> &[Concatenation] {
        &self.concatenations
    }

    /// Return the source span for the alternation.
    #[must_use]
    pub fn span(&self) -> &SourceSpan {
        &self.span
    }
}

/// A concatenation, separated by whitespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Concatenation {
    /// The repetition items in source order.
    repetitions: Vec<Repetition>,
    /// The source span of the concatenation.
    span: SourceSpan,
}

impl Concatenation {
    /// Construct a new concatenation.
    #[must_use]
    pub(crate) fn new(
        repetitions: Vec<Repetition>,
        span: SourceSpan,
    ) -> Self {
        Self { repetitions, span }
    }

    /// Return the repetitions in source order.
    #[must_use]
    pub fn repetitions(&self) -> &[Repetition] {
        &self.repetitions
    }

    /// Return the source span for the concatenation.
    #[must_use]
    pub fn span(&self) -> &SourceSpan {
        &self.span
    }
}

/// A repetition prefix and its element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repetition {
    /// The repetition prefix, if present.
    repeat: Option<Repeat>,
    /// The repeated element.
    element: AbnfElement,
    /// The source span of the repetition.
    span: SourceSpan,
}

impl Repetition {
    /// Construct a new repetition.
    #[must_use]
    pub(crate) fn new(
        repeat: Option<Repeat>,
        element: AbnfElement,
        span: SourceSpan,
    ) -> Self {
        Self {
            repeat,
            element,
            span,
        }
    }

    /// Return the repetition prefix, if present.
    #[must_use]
    pub fn repeat(&self) -> Option<&Repeat> {
        self.repeat.as_ref()
    }

    /// Return the repeated element.
    #[must_use]
    pub fn element(&self) -> &AbnfElement {
        &self.element
    }

    /// Return the source span for the repetition.
    #[must_use]
    pub fn span(&self) -> &SourceSpan {
        &self.span
    }
}

/// A repetition prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Repeat {
    /// An exact repetition count.
    Exact(u64),
    /// A bounded or unbounded range.
    Range(RepeatRange),
}

/// A range repetition prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepeatRange {
    /// The minimum repetition count.
    min: Option<u64>,
    /// The maximum repetition count.
    max: Option<u64>,
    /// The source span of the range text.
    span: SourceSpan,
}

impl RepeatRange {
    /// Construct a new repetition range.
    #[must_use]
    pub(crate) fn new(
        min: Option<u64>,
        max: Option<u64>,
        span: SourceSpan,
    ) -> Self {
        Self { min, max, span }
    }

    /// Return the minimum repetition count.
    #[must_use]
    pub fn min(&self) -> Option<u64> {
        self.min
    }

    /// Return the maximum repetition count.
    #[must_use]
    pub fn max(&self) -> Option<u64> {
        self.max
    }

    /// Return the source span for the range.
    #[must_use]
    pub fn span(&self) -> &SourceSpan {
        &self.span
    }
}

/// A parsed ABNF element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbnfElement {
    /// A reference to another rule name.
    RuleRef(Rulename),
    /// A grouped alternation.
    Group(GroupedAlternation),
    /// An optional alternation.
    Optional(AbnfOption),
    /// A quoted string literal.
    CharVal(CharVal),
    /// A numeric value.
    NumVal(NumVal),
    /// A prose description.
    ProseVal(ProseVal),
}

/// A grouped alternation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupedAlternation {
    /// The grouped alternation.
    alternation: Alternation,
    /// The source span of the group.
    span: SourceSpan,
}

impl GroupedAlternation {
    /// Construct a grouped alternation.
    #[must_use]
    pub(crate) fn new(
        alternation: Alternation,
        span: SourceSpan,
    ) -> Self {
        Self { alternation, span }
    }

    /// Return the inner alternation.
    #[must_use]
    pub fn alternation(&self) -> &Alternation {
        &self.alternation
    }

    /// Return the source span for the group.
    #[must_use]
    pub fn span(&self) -> &SourceSpan {
        &self.span
    }
}

/// An optional alternation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbnfOption {
    /// The optional alternation.
    alternation: Alternation,
    /// The source span of the option.
    span: SourceSpan,
}

impl AbnfOption {
    /// Construct a new optional alternation.
    #[must_use]
    pub(crate) fn new(
        alternation: Alternation,
        span: SourceSpan,
    ) -> Self {
        Self { alternation, span }
    }

    /// Return the inner alternation.
    #[must_use]
    pub fn alternation(&self) -> &Alternation {
        &self.alternation
    }

    /// Return the source span for the option.
    #[must_use]
    pub fn span(&self) -> &SourceSpan {
        &self.span
    }
}

/// A quoted string literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharVal {
    /// The literal contents without quotes.
    value: String,
    /// The source span of the literal.
    span: SourceSpan,
}

impl CharVal {
    /// Construct a quoted string literal.
    #[must_use]
    pub(crate) fn new(
        value: String,
        span: SourceSpan,
    ) -> Self {
        Self { value, span }
    }

    /// Return the literal contents without the surrounding quotes.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }

    /// Return the source span for the literal.
    #[must_use]
    pub fn span(&self) -> &SourceSpan {
        &self.span
    }
}

/// A prose description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProseVal {
    /// The prose contents without brackets.
    value: String,
    /// The source span of the prose value.
    span: SourceSpan,
}

impl ProseVal {
    /// Construct a prose description.
    #[must_use]
    pub(crate) fn new(
        value: String,
        span: SourceSpan,
    ) -> Self {
        Self { value, span }
    }

    /// Return the prose contents without the surrounding angle brackets.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }

    /// Return the source span for the prose value.
    #[must_use]
    pub fn span(&self) -> &SourceSpan {
        &self.span
    }
}

/// A numeric ABNF value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NumVal {
    /// The numeric base used by the literal.
    base: NumBase,
    /// The parsed numeric value payload.
    value: NumValue,
    /// The source span of the numeric literal.
    span: SourceSpan,
}

impl NumVal {
    /// Construct a numeric ABNF value.
    #[must_use]
    pub(crate) fn new(
        base: NumBase,
        value: NumValue,
        span: SourceSpan,
    ) -> Self {
        Self { base, value, span }
    }

    /// Return the numeric base.
    #[must_use]
    pub fn base(&self) -> NumBase {
        self.base
    }

    /// Return the parsed numeric value.
    #[must_use]
    pub fn value(&self) -> &NumValue {
        &self.value
    }

    /// Return the source span for the numeric value.
    #[must_use]
    pub fn span(&self) -> &SourceSpan {
        &self.span
    }
}

/// The base of a numeric value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumBase {
    /// Binary notation.
    Binary,
    /// Decimal notation.
    Decimal,
    /// Hexadecimal notation.
    Hexadecimal,
}

/// The parsed numeric payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NumValue {
    /// A list of numeric components.
    Sequence(Vec<u64>),
    /// A low/high range.
    Range(NumRange),
}

/// A numeric range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NumRange {
    /// The inclusive start of the range.
    start: u64,
    /// The inclusive end of the range.
    end: u64,
    /// The source span of the range.
    span: SourceSpan,
}

impl NumRange {
    /// Construct a numeric range.
    #[must_use]
    pub(crate) fn new(
        start: u64,
        end: u64,
        span: SourceSpan,
    ) -> Self {
        Self { start, end, span }
    }

    /// Return the start of the range.
    #[must_use]
    pub fn start(&self) -> u64 {
        self.start
    }

    /// Return the end of the range.
    #[must_use]
    pub fn end(&self) -> u64 {
        self.end
    }

    /// Return the source span for the range.
    #[must_use]
    pub fn span(&self) -> &SourceSpan {
        &self.span
    }
}

/// An ABNF parse or model-construction error.
#[derive(Debug, thiserror::Error)]
pub enum AbnfError {
    /// The Pest parser rejected the input.
    #[error("{0}")]
    Parse(#[from] Error<abnf::Rule>),
    /// The Pest parse tree could not be converted into the owned AST.
    #[error("{0}")]
    InvalidAst(String),
}

impl AbnfError {
    /// Construct a conversion error.
    #[must_use]
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::InvalidAst(message.into())
    }
}

/// An error that occurred while validating input against an ABNF document.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AbnfValidationError {
    /// The document does not contain any rules.
    #[error("ABNF document has no rules")]
    EmptyDocument,
    /// A referenced rule name was not found.
    #[error("unknown ABNF rule {name:?}")]
    UnknownRule {
        /// The missing rule name.
        name: String,
    },
    /// The validator encountered a recursive cycle at the same input offset.
    #[error("recursive ABNF rule {name:?} at byte offset {offset}")]
    RecursiveRule {
        /// The recursive rule name.
        name: String,
        /// The input byte offset where the recursion was detected.
        offset: usize,
    },
    /// ABNF prose values are not machine-checkable by this validator.
    #[error("unsupported ABNF prose value at span {span:?}")]
    UnsupportedProseValue {
        /// The source span for the prose value.
        span: SourceSpan,
    },
    /// The input did not match the ABNF grammar.
    #[error("input did not match ABNF at byte offset {offset} (expected rule {expected:?})")]
    Mismatch {
        /// The furthest matched input offset.
        offset: usize,
        /// The start rule name.
        expected: String,
    },
    /// The grammar contains an invalid numeric literal.
    #[error("invalid ABNF numeric literal: {message}")]
    InvalidNumericLiteral {
        /// The validation failure message.
        message: String,
    },
}

/// Stateful ABNF recognizer used by document validation.
struct AbnfValidator<'a> {
    /// The document being validated.
    document: &'a AbnfDocument,
    /// Maps rule names to indices in `document.rules`.
    rule_indices: HashMap<&'a str, usize>,
    /// Memoizes rule matches by rule index and input offset.
    memo: HashMap<(usize, usize), MemoState>,
}

/// Memoized state for a rule and input offset.
enum MemoState {
    /// The rule is currently being evaluated.
    InProgress,
    /// The rule has been evaluated and the matching offsets are cached.
    Done(BTreeSet<usize>),
}

impl<'a> AbnfValidator<'a> {
    /// Construct a validator for a document.
    fn new(document: &'a AbnfDocument) -> Self {
        let rule_indices = document
            .rules
            .iter()
            .enumerate()
            .map(|(index, rule)| (rule.name().as_str(), index))
            .collect::<HashMap<_, _>>();

        Self {
            document,
            rule_indices,
            memo: HashMap::new(),
        }
    }

    /// Match a rule by name at the given input position.
    fn match_rule(
        &mut self,
        name: &str,
        pos: usize,
        input: &[u8],
    ) -> Result<BTreeSet<usize>, AbnfValidationError> {
        let Some(&index) = self.rule_indices.get(name) else {
            return Err(AbnfValidationError::UnknownRule {
                name: name.to_owned(),
            });
        };

        self.match_rule_index(index, pos, input)
    }

    /// Match a rule by index at the given input position.
    fn match_rule_index(
        &mut self,
        index: usize,
        pos: usize,
        input: &[u8],
    ) -> Result<BTreeSet<usize>, AbnfValidationError> {
        let rule = self.document.rules.get(index).ok_or_else(|| {
            AbnfValidationError::InvalidNumericLiteral {
                message: "rule index was out of bounds".to_owned(),
            }
        })?;

        let key = (index, pos);
        if let Some(state) = self.memo.get(&key) {
            return match state {
                MemoState::InProgress => {
                    Err(AbnfValidationError::RecursiveRule {
                        name: rule.name().as_str().to_owned(),
                        offset: pos,
                    })
                },
                MemoState::Done(matches) => Ok(matches.clone()),
            };
        }

        self.memo.insert(key, MemoState::InProgress);
        let result = self.match_alternation(rule.expression(), pos, input);
        match result {
            Ok(matches) => {
                self.memo.insert(key, MemoState::Done(matches.clone()));
                Ok(matches)
            },
            Err(err) => {
                self.memo.remove(&key);
                Err(err)
            },
        }
    }

    /// Match an alternation at the given input position.
    fn match_alternation(
        &mut self,
        alternation: &Alternation,
        pos: usize,
        input: &[u8],
    ) -> Result<BTreeSet<usize>, AbnfValidationError> {
        let mut matches = BTreeSet::new();
        for concatenation in alternation.concatenations() {
            matches.extend(self.match_concatenation(concatenation, pos, input)?);
        }
        Ok(matches)
    }

    /// Match a concatenation at the given input position.
    fn match_concatenation(
        &mut self,
        concatenation: &Concatenation,
        pos: usize,
        input: &[u8],
    ) -> Result<BTreeSet<usize>, AbnfValidationError> {
        let mut positions = BTreeSet::from([pos]);
        for repetition in concatenation.repetitions() {
            let mut next_positions = BTreeSet::new();
            for current in positions {
                next_positions.extend(self.match_repetition(repetition, current, input)?);
            }
            if next_positions.is_empty() {
                return Ok(BTreeSet::new());
            }
            positions = next_positions;
        }
        Ok(positions)
    }

    /// Match a repetition at the given input position.
    fn match_repetition(
        &mut self,
        repetition: &Repetition,
        pos: usize,
        input: &[u8],
    ) -> Result<BTreeSet<usize>, AbnfValidationError> {
        match repetition.repeat() {
            None => self.match_element(repetition.element(), pos, input),
            Some(Repeat::Exact(count)) => {
                let count = usize::try_from(*count).map_err(|_| {
                    AbnfValidationError::InvalidNumericLiteral {
                        message: "repeat count exceeded usize".to_owned(),
                    }
                })?;
                self.match_repeated_element(repetition.element(), pos, count, Some(count), input)
            },
            Some(Repeat::Range(range)) => {
                let min = match range.min() {
                    Some(min) => {
                        usize::try_from(min).map_err(|_| {
                            AbnfValidationError::InvalidNumericLiteral {
                                message: "repeat minimum exceeded usize".to_owned(),
                            }
                        })?
                    },
                    None => 0,
                };
                let max = match range.max() {
                    Some(max) => {
                        Some(usize::try_from(max).map_err(|_| {
                            AbnfValidationError::InvalidNumericLiteral {
                                message: "repeat maximum exceeded usize".to_owned(),
                            }
                        })?)
                    },
                    None => None,
                };
                self.match_repeated_element(repetition.element(), pos, min, max, input)
            },
        }
    }

    /// Match a repeated element at the given input position.
    fn match_repeated_element(
        &mut self,
        element: &AbnfElement,
        pos: usize,
        min: usize,
        max: Option<usize>,
        input: &[u8],
    ) -> Result<BTreeSet<usize>, AbnfValidationError> {
        if let Some(max) = max
            && max < min
        {
            return Err(AbnfValidationError::InvalidNumericLiteral {
                message: "repeat maximum is smaller than minimum".to_owned(),
            });
        }

        let mut current = BTreeSet::from([pos]);
        for _ in 0..min {
            current = self.match_element_many(element, &current, input)?;
            if current.is_empty() {
                return Ok(BTreeSet::new());
            }
        }

        let mut matches = current.clone();
        let mut repeats = min;
        while max.is_none_or(|max| repeats < max) {
            let next = self.match_element_many(element, &current, input)?;
            if next.is_empty() || next == current {
                break;
            }
            matches.extend(next.iter().copied());
            current = next;
            repeats = repeats.checked_add(1).ok_or_else(|| {
                AbnfValidationError::InvalidNumericLiteral {
                    message: "repeat count exceeded usize".to_owned(),
                }
            })?;
        }

        Ok(matches)
    }

    /// Match an element against a set of candidate positions.
    fn match_element_many(
        &mut self,
        element: &AbnfElement,
        positions: &BTreeSet<usize>,
        input: &[u8],
    ) -> Result<BTreeSet<usize>, AbnfValidationError> {
        let mut matches = BTreeSet::new();
        for &pos in positions {
            matches.extend(self.match_element(element, pos, input)?);
        }
        Ok(matches)
    }

    /// Match a single element at the given input position.
    fn match_element(
        &mut self,
        element: &AbnfElement,
        pos: usize,
        input: &[u8],
    ) -> Result<BTreeSet<usize>, AbnfValidationError> {
        match element {
            AbnfElement::RuleRef(rule) => self.match_rule(rule.as_str(), pos, input),
            AbnfElement::Group(group) => self.match_alternation(group.alternation(), pos, input),
            AbnfElement::Optional(option) => {
                let mut matches = BTreeSet::from([pos]);
                matches.extend(self.match_alternation(option.alternation(), pos, input)?);
                Ok(matches)
            },
            AbnfElement::CharVal(value) => {
                Ok(Self::match_char_value(value.value().as_bytes(), pos, input))
            },
            AbnfElement::NumVal(value) => Self::match_num_value(value, pos, input),
            AbnfElement::ProseVal(value) => {
                Err(AbnfValidationError::UnsupportedProseValue {
                    span: value.span().clone(),
                })
            },
        }
    }

    /// Match a case-insensitive ABNF quoted string.
    fn match_char_value(
        value: &[u8],
        pos: usize,
        input: &[u8],
    ) -> BTreeSet<usize> {
        let Some(end) = pos.checked_add(value.len()) else {
            return BTreeSet::new();
        };

        let Some(candidate) = input.get(pos..end) else {
            return BTreeSet::new();
        };

        if candidate
            .iter()
            .copied()
            .zip(value.iter().copied())
            .all(|(left, right)| left.eq_ignore_ascii_case(&right))
        {
            BTreeSet::from([end])
        } else {
            BTreeSet::new()
        }
    }

    /// Match a numeric ABNF value at the given input position.
    fn match_num_value(
        value: &NumVal,
        pos: usize,
        input: &[u8],
    ) -> Result<BTreeSet<usize>, AbnfValidationError> {
        match value.value() {
            NumValue::Sequence(parts) => {
                let bytes = parts
                    .iter()
                    .map(|part| {
                        u8::try_from(*part).map_err(|_| {
                            AbnfValidationError::InvalidNumericLiteral {
                                message: format!(
                                    "numeric sequence value {part} exceeded byte range"
                                ),
                            }
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Self::match_exact_bytes(&bytes, pos, input))
            },
            NumValue::Range(range) => {
                let start = u8::try_from(range.start()).map_err(|_| {
                    AbnfValidationError::InvalidNumericLiteral {
                        message: format!(
                            "numeric range start {} exceeded byte range",
                            range.start()
                        ),
                    }
                })?;
                let end = u8::try_from(range.end()).map_err(|_| {
                    AbnfValidationError::InvalidNumericLiteral {
                        message: format!("numeric range end {} exceeded byte range", range.end()),
                    }
                })?;
                if start > end {
                    return Err(AbnfValidationError::InvalidNumericLiteral {
                        message: format!(
                            "numeric range start {} is greater than end {}",
                            range.start(),
                            range.end()
                        ),
                    });
                }

                let Some(&byte) = input.get(pos) else {
                    return Ok(BTreeSet::new());
                };
                if (start..=end).contains(&byte) {
                    let Some(next) = pos.checked_add(1) else {
                        return Err(AbnfValidationError::InvalidNumericLiteral {
                            message: "match position exceeded usize".to_owned(),
                        });
                    };
                    Ok(BTreeSet::from([next]))
                } else {
                    Ok(BTreeSet::new())
                }
            },
        }
    }

    /// Match an exact byte sequence at the given input position.
    fn match_exact_bytes(
        expected: &[u8],
        pos: usize,
        input: &[u8],
    ) -> BTreeSet<usize> {
        let Some(end) = pos.checked_add(expected.len()) else {
            return BTreeSet::new();
        };

        let Some(candidate) = input.get(pos..end) else {
            return BTreeSet::new();
        };

        if candidate == expected {
            BTreeSet::from([end])
        } else {
            BTreeSet::new()
        }
    }
}
