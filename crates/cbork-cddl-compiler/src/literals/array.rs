// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MPL-2.0

use std::convert::TryFrom;

use sprintf::{Printf, vsprintf};
use thiserror::Error;

use crate::{
    literals::{byte::ByteLiteralBytes, text::TextLiteralBytes},
    node::WrappedNode,
    resolver_cache::{EntryState, ResolverCache},
};

/// Opaque literal element stored in a literal array.
#[derive(Debug, Clone, PartialEq)]
pub enum LiteralValue {
    /// A text literal.
    Text(TextLiteralBytes),
    /// A byte literal.
    Bytes(ByteLiteralBytes),
    /// An integer literal.
    Integer(i128),
    /// A floating-point literal.
    Float(f64),
}

impl Eq for LiteralValue {}

/// Error returned by literal-array collection and construction.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LiteralArrayError {
    /// The RHS being examined is not array-shaped.
    #[error("RHS is not an array")]
    NotArray,
    /// A non-text/byte literal was encountered in a join operation.
    #[error("literal at index {index} is not a string")]
    NotJoinable {
        /// The literal index that failed the joinability check.
        index: usize,
    },
    /// A non-text/byte literal was encountered where text was required.
    #[error("literal at index {index} is not text")]
    NotText {
        /// The literal index that failed the text check.
        index: usize,
    },
    /// A byte literal could not be promoted to text.
    #[error("literal cannot be promoted to text: {0}")]
    InvalidTextLiteral(String),
    /// The `printf` controller array is empty.
    #[error("printf requires a format string")]
    EmptyPrintf,
    /// A literal could not be converted into a printf argument.
    #[error("literal at index {index} is not supported by printf")]
    UnsupportedPrintfValue {
        /// The literal index that failed the printf conversion.
        index: usize,
    },
    /// The printf formatter rejected the provided format string or arguments.
    #[error("printf failed: {0}")]
    PrintfError(#[from] sprintf::PrintfError),
}

/// Opaque collection of literal values used by `.join` / `.printf`.
#[derive(Debug, Clone, PartialEq)]
pub struct LiteralArray(Vec<LiteralValue>);

impl Eq for LiteralArray {}

impl LiteralArray {
    /// Inspect an AST subtree and collect a literal array from it.
    ///
    /// Returns:
    /// - `Err(NotArray)` if the subtree is not array-shaped
    /// - `Ok(None)` if the subtree is array-shaped but contains a non-literal element
    /// - `Ok(Some(...))` if every element is a literal value
    ///
    /// # Errors
    ///
    /// Returns [`LiteralArrayError::NotArray`] if the node is not an
    /// array.
    pub fn new(
        node: &WrappedNode,
        cache: &mut ResolverCache,
    ) -> Result<Option<Self>, LiteralArrayError> {
        let Some(group) = find_array_group(node) else {
            return Err(LiteralArrayError::NotArray);
        };

        let mut values = Vec::new();
        let mut saw_element = false;

        for element in iter_grpent_nodes(group) {
            saw_element = true;
            let Some(value) = literal_from_grpent(element, cache) else {
                return Ok(None);
            };
            values.push(value);
        }

        if !saw_element {
            return Ok(Some(Self(values)));
        }

        Ok(Some(Self(values)))
    }

    /// Join the collected literal payloads as bytes, returning a
    /// diagnostic-friendly error when a value is not joinable.
    pub fn try_join_bytes(&self) -> Result<ByteLiteralBytes, LiteralArrayError> {
        let mut joined: Option<ByteLiteralBytes> = None;

        for (index, value) in self.0.iter().enumerate() {
            let piece = match value {
                LiteralValue::Text(text) => ByteLiteralBytes::from(text.clone()),
                LiteralValue::Bytes(bytes) => bytes.clone(),
                _ => {
                    return Err(LiteralArrayError::NotJoinable { index });
                },
            };

            joined = Some(match joined {
                Some(acc) => acc.cat(&piece),
                None => piece,
            });
        }

        Ok(joined.unwrap_or_else(|| ByteLiteralBytes::from_bytes(Vec::new())))
    }

    /// Join the collected literal payloads as text.
    ///
    /// The bytes are concatenated first and only the final joined value
    /// is validated as text.
    pub fn join_text(&self) -> Result<TextLiteralBytes, LiteralArrayError> {
        let joined = self.try_join_bytes()?;
        TextLiteralBytes::try_from(joined)
            .map_err(|e| LiteralArrayError::InvalidTextLiteral(e.to_string()))
    }

    /// Format the collected literal payloads using printf-style rules.
    ///
    /// The first literal is the format string. The remaining literals
    /// are supplied as dynamically typed printf arguments.
    pub fn printf(&self) -> Result<TextLiteralBytes, LiteralArrayError> {
        let Some((format, args)) = self.0.split_first() else {
            return Err(LiteralArrayError::EmptyPrintf);
        };

        let format = literal_to_text(format, 0)?;
        let format = String::from_utf8(format.into_bytes())
            .map_err(|e| LiteralArrayError::InvalidTextLiteral(e.to_string()))?;

        let mut owned_args: Vec<Box<dyn Printf>> = Vec::with_capacity(args.len());
        for (index, value) in args.iter().enumerate() {
            #[allow(
                clippy::arithmetic_side_effects,
                reason = "indexing safe due to bounded iteration."
            )]
            owned_args.push(literal_to_printf_arg(value, index + 1)?);
        }

        let refs: Vec<&dyn Printf> = owned_args.iter().map(std::convert::AsRef::as_ref).collect();
        let formatted = vsprintf(&format, &refs)?;
        Ok(TextLiteralBytes::from_bytes(formatted.into_bytes()))
    }

    /// Return the collected literal values.
    #[must_use]
    pub fn as_slice(&self) -> &[LiteralValue] {
        &self.0
    }
}

/// Find the array `group` subtree under a typed array expression.
fn find_array_group(node: &WrappedNode) -> Option<&WrappedNode> {
    match node {
        WrappedNode::RuleLine { children, .. }
        | WrappedNode::Syntax { children, .. }
        | WrappedNode::Directive { children, .. } => {
            if let WrappedNode::Syntax {
                rule,
                text,
                children,
                ..
            } = node
                && rule == "type2"
                && text.trim_start().starts_with('[')
            {
                return children.iter().find(
                    |child| matches!(child, WrappedNode::Syntax { rule, .. } if rule == "group"),
                );
            }

            if let WrappedNode::Syntax { rule, .. } = node
                && rule == "group"
            {
                return Some(node);
            }

            children.iter().find_map(find_array_group)
        },
        _ => None,
    }
}

/// Iterate the `grpent` nodes that make up a group.
fn iter_grpent_nodes(node: &WrappedNode) -> Vec<&WrappedNode> {
    let mut out = Vec::new();
    collect_grpent_nodes(node, &mut out);
    out
}

/// Collect all `grpent` and `groupname` nodes from an AST subtree
/// into `out`.
fn collect_grpent_nodes<'a>(
    node: &'a WrappedNode,
    out: &mut Vec<&'a WrappedNode>,
) {
    match node {
        WrappedNode::Syntax { rule, children, .. } if rule == "grpent" => {
            out.push(node);
            for child in children {
                collect_grpent_nodes(child, out);
            }
        },
        WrappedNode::Syntax { children, .. } => {
            for child in children {
                collect_grpent_nodes(child, out);
            }
        },
        _ => {},
    }
}

/// Extract a literal value from a `grpent` node if possible.
fn literal_from_grpent(
    node: &WrappedNode,
    cache: &mut ResolverCache,
) -> Option<LiteralValue> {
    let WrappedNode::Syntax { children, .. } = node else {
        return None;
    };

    let mut direct_literal: Option<LiteralValue> = None;

    for child in children {
        match child {
            WrappedNode::Syntax { rule, .. } if rule == "memberkey" || rule == "group" => {
                return None;
            },
            WrappedNode::Syntax { rule, .. }
                if rule == "ctlop" || rule == "rangeop" || rule == "occur" =>
            {
                return None;
            },
            WrappedNode::Syntax { rule, text, .. } if rule == "value" => {
                direct_literal = literal_from_value(child, cache);
            },
            WrappedNode::Syntax { rule, text, .. } if rule == "typename" || rule == "groupname" => {
                direct_literal = literal_from_typename(text, cache);
            },
            WrappedNode::Syntax { .. } => {
                if let Some(literal) = literal_from_nested(child, cache) {
                    direct_literal = Some(literal);
                }
            },
            _ => {},
        }
    }

    direct_literal
}

/// Resolve a nested subtree to a literal, if possible.
fn literal_from_nested(
    node: &WrappedNode,
    cache: &mut ResolverCache,
) -> Option<LiteralValue> {
    match node {
        WrappedNode::Syntax {
            rule,
            text,
            children,
            ..
        } => {
            match rule.as_str() {
                "value" => literal_from_value(node, cache),
                "typename" | "groupname" => literal_from_typename(text, cache),
                _ => {
                    for child in children {
                        if let Some(literal) = literal_from_nested(child, cache) {
                            return Some(literal);
                        }
                    }
                    None
                },
            }
        },
        _ => None,
    }
}

/// Convert a `value` subtree into a literal wrapper.
fn literal_from_value(
    node: &WrappedNode,
    _cache: &mut ResolverCache,
) -> Option<LiteralValue> {
    let WrappedNode::Syntax { children, .. } = node else {
        return None;
    };

    for child in children {
        if let WrappedNode::Syntax { rule, text, .. } = child {
            return match rule.as_str() {
                "uint" | "int" => text.trim().parse::<i128>().ok().map(LiteralValue::Integer),
                "intfloat" | "number" => {
                    let trimmed = text.trim();
                    if trimmed.contains('.') || trimmed.contains('e') || trimmed.contains('E') {
                        trimmed.parse::<f64>().ok().map(LiteralValue::Float)
                    } else {
                        trimmed.parse::<i128>().ok().map(LiteralValue::Integer)
                    }
                },
                "hexfloat" => text.trim().parse::<f64>().ok().map(LiteralValue::Float),
                "text" => {
                    TextLiteralBytes::parse(text.as_bytes())
                        .ok()
                        .map(LiteralValue::Text)
                },
                "bytes" => {
                    ByteLiteralBytes::parse(text.as_bytes())
                        .ok()
                        .map(LiteralValue::Bytes)
                },
                _ => None,
            };
        }
    }

    None
}

/// Convert a typename reference to a literal wrapper using the cache.
fn literal_from_typename(
    text: &str,
    cache: &mut ResolverCache,
) -> Option<LiteralValue> {
    match cache.get(text.trim()) {
        EntryState::Text(t) => Some(LiteralValue::Text(t.clone())),
        EntryState::Bytes(b) => Some(LiteralValue::Bytes(b.clone())),
        EntryState::Integer(i) => Some(LiteralValue::Integer(*i)),
        EntryState::Float(f) => Some(LiteralValue::Float(*f)),
        _ => None,
    }
}

/// Convert a literal value to its text representation for .printf.
fn literal_to_text(
    value: &LiteralValue,
    index: usize,
) -> Result<TextLiteralBytes, LiteralArrayError> {
    match value {
        LiteralValue::Text(text) => Ok(text.clone()),
        LiteralValue::Bytes(bytes) => {
            TextLiteralBytes::try_from(bytes.clone())
                .map_err(|e| LiteralArrayError::InvalidTextLiteral(e.to_string()))
        },
        _ => Err(LiteralArrayError::NotText { index }),
    }
}

/// Convert a literal value to a printf argument.
fn literal_to_printf_arg(
    value: &LiteralValue,
    index: usize,
) -> Result<Box<dyn Printf>, LiteralArrayError> {
    match value {
        LiteralValue::Text(text) => {
            let s = String::from_utf8(text.as_ref().to_vec())
                .map_err(|e| LiteralArrayError::InvalidTextLiteral(e.to_string()))?;
            Ok(Box::new(s))
        },
        LiteralValue::Bytes(bytes) => {
            let text = TextLiteralBytes::try_from(bytes.clone())
                .map_err(|e| LiteralArrayError::InvalidTextLiteral(e.to_string()))?;
            let s = String::from_utf8(text.into_bytes())
                .map_err(|e| LiteralArrayError::InvalidTextLiteral(e.to_string()))?;
            Ok(Box::new(s))
        },
        LiteralValue::Integer(i) => {
            let narrowed = i64::try_from(*i)
                .map_err(|_| LiteralArrayError::UnsupportedPrintfValue { index })?;
            Ok(Box::new(narrowed))
        },
        LiteralValue::Float(f) => Ok(Box::new(*f)),
    }
}

#[cfg(test)]
mod tests {
    use cbork_cddl_parser::parse_cddl;

    use super::*;
    use crate::{
        node::WrappedNode,
        preprocessor::{inject_directives, process_ast},
    };

    fn parse_nodes(input: &str) -> Vec<WrappedNode> {
        let ast = parse_cddl(input).unwrap();
        let ast = process_ast(ast).unwrap();
        inject_directives(std::path::Path::new("test.cddl"), &ast, input).unwrap()
    }

    #[test]
    fn new_rejects_non_arrays() {
        let nodes = parse_nodes("a = 1\n");
        let mut cache = ResolverCache::new();
        let result = LiteralArray::new(&nodes[0], &mut cache);
        assert!(matches!(result, Err(LiteralArrayError::NotArray)));
    }

    #[test]
    fn new_collects_literals_from_array() {
        let nodes = parse_nodes("a = [1, \"x\", 'y']\n");
        let mut cache = ResolverCache::new();
        let result = LiteralArray::new(&nodes[0], &mut cache).unwrap();
        let array = result.expect("literal array");
        assert_eq!(array.as_slice().len(), 3);
        assert!(matches!(array.as_slice()[0], LiteralValue::Integer(1)));
        assert!(matches!(array.as_slice()[1], LiteralValue::Text(_)));
        assert!(matches!(array.as_slice()[2], LiteralValue::Bytes(_)));
    }

    #[test]
    fn new_rejects_array_with_non_literals() {
        let nodes = parse_nodes("a = [1, foo]\n");
        let mut cache = ResolverCache::new();
        let result = LiteralArray::new(&nodes[0], &mut cache).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn new_resolves_named_literals_from_cache() {
        let nodes = parse_nodes("a = [x, y]\n");
        let mut cache = ResolverCache::new();
        cache.resolve("x", EntryState::Integer(7)).unwrap();
        cache
            .resolve(
                "y",
                EntryState::Text(
                    crate::literals::text::TextLiteralBytes::parse(br#""ok""#).unwrap(),
                ),
            )
            .unwrap();

        let result = LiteralArray::new(&nodes[0], &mut cache).unwrap();
        let array = result.expect("literal array");
        assert_eq!(array.as_slice().len(), 2);
        assert!(matches!(array.as_slice()[0], LiteralValue::Integer(7)));
        assert!(matches!(array.as_slice()[1], LiteralValue::Text(_)));
    }

    #[test]
    fn join_bytes_concatenates_strings() {
        let nodes = parse_nodes("a = [\"hi\", 'there']\n");
        let mut cache = ResolverCache::new();
        let array = LiteralArray::new(&nodes[0], &mut cache).unwrap().unwrap();
        assert_eq!(join_bytes_for_test(&array).as_ref(), b"hithere");
        assert_eq!(array.join_text().unwrap().as_ref(), b"hithere");
    }

    #[test]
    fn printf_formats_literals() {
        let nodes = parse_nodes("a = [\"%d %s\", 7, 'ok']\n");
        let mut cache = ResolverCache::new();
        let array = LiteralArray::new(&nodes[0], &mut cache).unwrap().unwrap();
        assert_eq!(array.printf().unwrap().as_ref(), b"7 ok");
    }

    fn join_bytes_for_test(array: &LiteralArray) -> ByteLiteralBytes {
        array
            .try_join_bytes()
            .expect("test literal array should join")
    }
}
