// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MPL-2.0

//! Rule and symbol classification helpers.
//!
//! These helpers keep CDDL type/group/socket semantics out of ad hoc string
//! parsing in later compiler passes.

use crate::WrappedNode;

/// The syntactic assignment operator used by a rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignmentKind {
    /// `=`
    Define,
    /// `/=`
    TypeAugment,
    /// `//=`
    GroupAugment,
}

/// The namespace/kind implied by a rule head or reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    /// Ordinary type name.
    Type,
    /// Ordinary group name.
    Group,
    /// `$name`, a type socket.
    TypeSocket,
    /// `$$name`, a group socket.
    GroupSocket,
}

impl SymbolKind {
    /// Return whether this symbol is a socket.
    #[must_use]
    pub fn is_socket(self) -> bool {
        matches!(self, Self::TypeSocket | Self::GroupSocket)
    }
}

/// A classified top-level rule head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleHead {
    /// Rule name text.
    pub name: String,
    /// Rule symbol kind.
    pub kind: SymbolKind,
    /// Assignment operator used by this rule.
    pub assignment: AssignmentKind,
}

/// Classify a rule line's left-hand side and assignment operator.
#[must_use]
pub fn rule_head(node: &WrappedNode) -> Option<RuleHead> {
    let WrappedNode::RuleLine { children, .. } = node else {
        return None;
    };

    rule_head_from_children(children)
}

/// Classify a rule line from its child syntax nodes.
#[must_use]
pub(crate) fn rule_head_from_children(children: &[WrappedNode]) -> Option<RuleHead> {
    children.iter().find_map(rule_head_in_node)
}

/// Return the top-level rule name for a rule line.
#[must_use]
pub fn rule_name(node: &WrappedNode) -> Option<String> {
    rule_head(node).map(|head| head.name)
}

/// Classify a syntax node that names a type or group.
#[must_use]
pub fn symbol_kind(
    rule: &str,
    text: &str,
) -> Option<SymbolKind> {
    let text = text.trim();
    if text.starts_with("$$") {
        return Some(SymbolKind::GroupSocket);
    }
    if text.starts_with('$') {
        return Some(SymbolKind::TypeSocket);
    }

    match rule {
        "typename" => Some(SymbolKind::Type),
        "groupname" => Some(SymbolKind::Group),
        _ => None,
    }
}

/// Extracts a [`RuleHead`] from a syntax node if it represents a rule definition.
fn rule_head_in_node(node: &WrappedNode) -> Option<RuleHead> {
    let WrappedNode::Syntax { rule, children, .. } = node else {
        return None;
    };

    if rule != "expr" {
        return children.iter().find_map(rule_head_in_node);
    }

    let mut name: Option<(String, SymbolKind)> = None;
    let mut assignment = None;

    for child in children {
        let WrappedNode::Syntax {
            rule,
            text,
            children,
            ..
        } = child
        else {
            continue;
        };

        match rule.as_str() {
            "typename" | "groupname" => {
                let kind = symbol_kind(rule, text)?;
                name = Some((text.trim().to_owned(), kind));
            },
            "assignt" => {
                assignment = Some(if text.trim() == "/=" {
                    AssignmentKind::TypeAugment
                } else {
                    AssignmentKind::Define
                });
            },
            "assigng" => {
                assignment = Some(if text.trim() == "//=" {
                    AssignmentKind::GroupAugment
                } else {
                    AssignmentKind::Define
                });
            },
            _ => {
                if name.is_none()
                    && let Some(head) = children.iter().find_map(rule_head_in_node)
                {
                    return Some(head);
                }
            },
        }
    }

    let (name, kind) = name?;
    Some(RuleHead {
        name,
        kind,
        assignment: assignment?,
    })
}
