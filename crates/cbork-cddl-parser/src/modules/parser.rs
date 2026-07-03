// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Directive comment parser.
//!
//! Parses CDDL module directive comments (`;# import ...`, `;# include ...`)
//! into structured [`Directive`] values.
//!
//! Based on the ABNF in draft-ietf-cbor-cddl-modules:
//! ```abnf
//! directive = ";#" RS (%s"import" / %s"include") RS [from-clause]
//!                       filename [as-clause] CRLF
//! from-clause = 1*(id-or-all [","] RS) %s"from" RS
//! as-clause = RS %s"as" RS id
//! ```

use super::directives::{Directive, FileName};

/// Parse an ordered list of [`Directive`] values from a block of comment text.
///
/// Lines that do not start with `;#` followed by whitespace are silently
/// ignored.  TAB is treated as equivalent to SP between directive fields.
///
/// # Examples
///
/// ```
/// # use cbork_cddl_parser::modules::parse_directives;
/// let dirs = parse_directives(";# import rfc9052\n").unwrap();
/// assert_eq!(dirs.len(), 1);
/// ```
///
/// # Errors
///
/// Returns an error if a directive line cannot be parsed.
pub fn parse_directives(input: &str) -> Result<Vec<Directive>, DirectiveParseError> {
    let mut directives = Vec::new();

    for (line_num_offset, line) in input.lines().enumerate() {
        let trimmed = line.trim_start();
        if !is_directive_line(trimmed) {
            continue;
        }

        let directive = parse_directive_line(trimmed).map_err(|kind| {
            DirectiveParseError {
                line: line_num_offset,
                kind,
            }
        })?;
        directives.push(directive);
    }

    Ok(directives)
}

/// Error returned when a directive line fails to parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectiveParseError {
    /// Zero-based line index within the input block.
    pub line: usize,
    /// The kind of parse failure.
    pub kind: DirectiveParseErrorKind,
}

/// Specific reason a directive line could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectiveParseErrorKind {
    /// The keyword was not `import` or `include`.
    UnknownKeyword(String),
    /// No filename was found after the keyword.
    MissingFilename,
    /// The from-clause was malformed.
    MalformedFromClause,
    /// The `as` clause was missing an alias name.
    MissingAlias,
}

impl core::fmt::Display for DirectiveParseError {
    fn fmt(
        &self,
        f: &mut core::fmt::Formatter<'_>,
    ) -> core::fmt::Result {
        write!(
            f,
            "directive parse error at line {}: {}",
            self.line,
            match &self.kind {
                DirectiveParseErrorKind::UnknownKeyword(kw) =>
                    format!("unknown directive keyword '{kw}'"),
                DirectiveParseErrorKind::MissingFilename => "missing filename".to_owned(),
                DirectiveParseErrorKind::MalformedFromClause => "malformed from-clause".to_owned(),
                DirectiveParseErrorKind::MissingAlias =>
                    "expected alias name after 'as'".to_owned(),
            }
        )
    }
}

/// Returns `true` if `line` starts with `;#` followed by at least one
/// whitespace character (space or tab).
fn is_directive_line(line: &str) -> bool {
    line.strip_prefix(";#")
        .is_some_and(|rest| rest.starts_with([' ', '\t']))
}

/// Parse a single directive line (already stripped of leading whitespace and
/// confirmed to start with `;#` followed by whitespace).
fn parse_directive_line(line: &str) -> Result<Directive, DirectiveParseErrorKind> {
    // Strip the `;#` prefix and leading whitespace
    let body = line
        .strip_prefix(";#")
        .ok_or_else(|| DirectiveParseErrorKind::UnknownKeyword(String::new()))?;
    let body = body.trim_start_matches([' ', '\t']);

    // Read the keyword
    let (keyword, rest) = take_ws_token(body);
    let rest = skip_ws(rest);

    let is_import = match keyword {
        "import" => true,
        "include" => false,
        other => return Err(DirectiveParseErrorKind::UnknownKeyword(other.to_owned())),
    };

    // Check for from-clause: scan forward for the standalone word "from"
    let (names, rest) = if let Some((names, after_from)) = try_parse_from_clause(rest) {
        (names, after_from)
    } else {
        (Vec::new(), rest)
    };

    // Parse filename
    let rest = skip_ws(rest);
    if rest.is_empty() {
        return Err(DirectiveParseErrorKind::MissingFilename);
    }

    let (filename_ref, rest) = if rest.as_bytes().first().copied() == Some(b'"') {
        parse_quoted_filename(rest)
    } else {
        let (fname, remaining) = take_filename(rest);
        if fname.is_empty() {
            return Err(DirectiveParseErrorKind::MissingFilename);
        }
        (fname, remaining)
    };
    let filename = FileName::parse(filename_ref);

    // Check for optional as-clause
    let rest = skip_ws(rest);
    let alias = parse_as_clause(rest)?;

    Ok(build_directive(is_import, names, filename, alias))
}

/// Try to parse an optional `as <id>` clause.  Returns `None` if no
/// as-clause is present.  Returns an error if `as` is present but the alias
/// name is missing.
fn parse_as_clause(rest: &str) -> Result<Option<String>, DirectiveParseErrorKind> {
    let Some(rest_after_as) = rest.strip_prefix("as") else {
        return Ok(None);
    };

    // "as" must be followed by whitespace or end of input.
    // "as" not followed by whitespace means it's part of the filename.
    if rest_after_as.is_empty() || rest_after_as.starts_with([' ', '\t']) {
        let trimmed = skip_ws(rest_after_as);
        let (alias_name, _) = take_id(trimmed);
        if alias_name.is_empty() {
            return Err(DirectiveParseErrorKind::MissingAlias);
        }
        Ok(Some(alias_name.to_owned()))
    } else {
        Ok(None)
    }
}

/// Try to parse a from-clause: `name, name, ... from `.
///
/// Returns `Some((names, rest))` if a from-clause was found, where `rest` is
/// the input after the from-clause.  Returns `None` if no from-clause is
/// present.
fn try_parse_from_clause(input: &str) -> Option<(Vec<String>, &str)> {
    let bytes = input.as_bytes();
    let mut pos: usize = 0;
    let len = bytes.len();
    let mut names: Vec<String> = Vec::new();

    while pos < len {
        // Skip whitespace
        while pos < len && is_ws(*bytes.get(pos)?) {
            pos = pos.wrapping_add(1);
        }

        if pos >= len {
            return None;
        }

        // Check if we're at "from" followed by whitespace or end
        let tail = input.get(pos..)?;
        if tail.starts_with("from") {
            let after_from = pos.wrapping_add(4);
            let is_word_boundary =
                after_from >= len || bytes.get(after_from).copied().is_some_and(is_ws);
            if is_word_boundary {
                if names.is_empty() {
                    return None;
                }
                let rest = input.get(after_from..)?;
                let rest = skip_ws(rest);
                return Some((names, rest));
            }
        }

        // Read a name token
        let slice = input.get(pos..)?;
        let (name, rest) = take_selector_or_all(slice);
        if name.is_empty() {
            return None;
        }
        names.push(name.to_owned());

        // Check for optional comma
        let rest = skip_ws(rest);
        pos = if rest.starts_with(',') {
            len.wrapping_sub(rest.len()).wrapping_add(1)
        } else {
            len.wrapping_sub(rest.len())
        };
    }

    None
}

/// Build the appropriate [`Directive`] variant from parsed fields.
fn build_directive(
    is_import: bool,
    names: Vec<String>,
    filename: FileName,
    alias: Option<String>,
) -> Directive {
    let has_names = !names.is_empty();
    match (is_import, has_names, alias) {
        (true, false, None) => Directive::Import { filename },
        (true, false, Some(alias)) => Directive::ImportAs { filename, alias },
        (true, true, None) => Directive::ImportFrom { names, filename },
        (true, true, Some(alias)) => {
            Directive::ImportFromAs {
                names,
                filename,
                alias,
            }
        },
        (false, false, None) => Directive::Include { filename },
        (false, false, Some(alias)) => Directive::IncludeAs { filename, alias },
        (false, true, None) => Directive::IncludeFrom { names, filename },
        (false, true, Some(alias)) => {
            Directive::IncludeFromAs {
                names,
                filename,
                alias,
            }
        },
    }
}

/// Consume leading whitespace (space or tab) and return the rest of the
/// string.
fn skip_ws(s: &str) -> &str {
    s.trim_start_matches([' ', '\t'])
}

/// Check if a byte is whitespace (space or tab).
const fn is_ws(b: u8) -> bool {
    b == b' ' || b == b'\t'
}

/// Take a whitespace-delimited token from the front of the input.
fn take_ws_token(s: &str) -> (&str, &str) {
    let end = s.find([' ', '\t']).unwrap_or(s.len());
    let (head, tail) = split_str_at(s, end);
    (head, tail)
}

/// Take a filename token: alphanumeric plus `-._` (ABNF `filename` rule).
fn take_filename(s: &str) -> (&str, &str) {
    let end = s
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_'))
        .unwrap_or(s.len());
    split_str_at(s, end)
}

/// Take a directive selector token: an identifier-like name or `*`.
///
/// Selectors need to support:
/// - dotted imported names such as `cose.label`
/// - hyphenated names such as `untagged-argon2id`
/// - generic rule forms such as `foo<t>` or `foo<a, b>`
fn take_selector_or_all(s: &str) -> (&str, &str) {
    if let Some(rest) = s.strip_prefix('*') {
        return (s.get(..1).unwrap_or(""), rest);
    }
    take_selector(s)
}

/// Take a directive selector token.
fn take_selector(s: &str) -> (&str, &str) {
    let bytes = s.as_bytes();
    let Some(&first) = bytes.first() else {
        return ("", s);
    };

    if !is_id_start(first) {
        return ("", s);
    }

    let mut end: usize = 1;
    let mut generic_depth: usize = 0;

    while let Some(&byte) = bytes.get(end) {
        match byte {
            b'<' => {
                generic_depth = generic_depth.wrapping_add(1);
                end = end.wrapping_add(1);
            },
            b'>' => {
                if generic_depth == 0 {
                    break;
                }
                generic_depth = generic_depth.saturating_sub(1);
                end = end.wrapping_add(1);
            },
            b',' | b' ' | b'\t' if generic_depth == 0 => break,
            _ if is_selector_continue(byte) || (generic_depth > 0 && is_selector_generic(byte)) => {
                end = end.wrapping_add(1);
            },
            _ => break,
        }
    }

    split_str_at(s, end)
}

/// Take an id token per ABNF:
/// ```abnf
/// id = ("$" / %x40-5a / "_" / %x61-7a)
///      *("$" / %x30-39 / %x40-5a / "_" / %x61-7a)
/// ```
fn take_id(s: &str) -> (&str, &str) {
    let bytes = s.as_bytes();
    let Some(&first) = bytes.first() else {
        return ("", s);
    };

    if !is_id_start(first) {
        return ("", s);
    }

    let mut end: usize = 1;
    while end < bytes.len() && bytes.get(end).copied().is_some_and(is_id_continue) {
        end = end.wrapping_add(1);
    }
    split_str_at(s, end)
}

/// Returns `true` if `b` is a valid first character of an ABNF `id`.
const fn is_id_start(b: u8) -> bool {
    matches!(b, b'$' | b'@' | b'A'..=b'Z' | b'_' | b'a'..=b'z')
}

/// Returns `true` if `b` is a valid continuation character of an ABNF `id`.
///
/// Includes `.` as a local extension to support dotted namespaced
/// identifiers (e.g. `cose.label`) used in spec examples.
const fn is_id_continue(b: u8) -> bool {
    matches!(
        b,
        b'$' | b'.' | b'0'..=b'9' | b'@' | b'A'..=b'Z' | b'_' | b'a'..=b'z'
    )
}

/// Returns `true` if `b` is valid outside generic angle brackets in a
/// directive selector.
const fn is_selector_continue(b: u8) -> bool {
    is_id_continue(b) || b == b'-'
}

/// Returns `true` if `b` is valid inside generic angle brackets in a
/// directive selector.
const fn is_selector_generic(b: u8) -> bool {
    is_selector_continue(b) || matches!(b, b',' | b' ' | b'\t')
}

/// Parse a quoted filename like `"./some/path.cddl"` or
/// `"/absolute/path.cddl"`.
///
/// Supports percent-encoded characters in quoted paths as a local extension.
fn parse_quoted_filename(s: &str) -> (&str, &str) {
    let inner = s.get(1..).unwrap_or("");
    let end = inner.find('"').unwrap_or(inner.len());
    let total_len = end.wrapping_add(2);
    let (head, tail) = split_str_at(s, total_len);
    (head, tail)
}

/// Split a string at `mid` byte index.  Panics if `mid` is not a char
/// boundary, but all callers use indices from `find()` or byte counting on
/// ASCII content.
fn split_str_at(
    s: &str,
    mid: usize,
) -> (&str, &str) {
    // Safety: mid is always at a valid UTF-8 boundary because it comes from
    // `find()`, `len()`, or byte-advancing over ASCII characters.
    if mid >= s.len() {
        return (s, "");
    }
    (s.get(..mid).unwrap_or(s), s.get(mid..).unwrap_or(""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::directives::FileName;

    #[test]
    fn parse_import() {
        let dirs = parse_directives(";# import rfc9052").unwrap();
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0], Directive::Import {
            filename: FileName::parse("rfc9052")
        });
    }

    #[test]
    fn parse_import_as() {
        let dirs = parse_directives(";# import rfc9052 as cose").unwrap();
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0], Directive::ImportAs {
            filename: FileName::parse("rfc9052"),
            alias: "cose".to_owned()
        });
    }

    #[test]
    fn parse_include() {
        let dirs = parse_directives(";# include rfc9052").unwrap();
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0], Directive::Include {
            filename: FileName::parse("rfc9052")
        });
    }

    #[test]
    fn parse_include_as() {
        let dirs = parse_directives(";# include rfc9052 as cose").unwrap();
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0], Directive::IncludeAs {
            filename: FileName::parse("rfc9052"),
            alias: "cose".to_owned()
        });
    }

    #[test]
    fn parse_include_from() {
        let dirs = parse_directives(";# include label, values from rfc9052").unwrap();
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0], Directive::IncludeFrom {
            names: vec!["label".to_owned(), "values".to_owned()],
            filename: FileName::parse("rfc9052")
        });
    }

    #[test]
    fn parse_include_from_as() {
        let dirs =
            parse_directives(";# include cose.label, cose.values from rfc9052 as cose").unwrap();
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0], Directive::IncludeFromAs {
            names: vec!["cose.label".to_owned(), "cose.values".to_owned()],
            filename: FileName::parse("rfc9052"),
            alias: "cose".to_owned()
        });
    }

    #[test]
    fn parse_import_from_as_with_hyphenated_generic_name() {
        let dirs = parse_directives(
            ";# import a2d.untagged-argon2id<t> from \"../../argon2id/doc/argon2id.cddl\" as a2d",
        )
        .unwrap();
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0], Directive::ImportFromAs {
            names: vec!["a2d.untagged-argon2id<t>".to_owned()],
            filename: FileName::parse("\"../../argon2id/doc/argon2id.cddl\""),
            alias: "a2d".to_owned(),
        });
    }

    #[test]
    fn parse_import_from() {
        let dirs = parse_directives(";# import MyRule from mymodule").unwrap();
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0], Directive::ImportFrom {
            names: vec!["MyRule".to_owned()],
            filename: FileName::parse("mymodule")
        });
    }

    #[test]
    fn parse_wildcard_from() {
        let dirs = parse_directives(";# include * from rfc9052").unwrap();
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0], Directive::IncludeFrom {
            names: vec!["*".to_owned()],
            filename: FileName::parse("rfc9052")
        });
    }

    #[test]
    fn parse_multiple_directives() {
        let input = ";# import rfc9052 as cose\n;# include * from rfc8610\n";
        let dirs = parse_directives(input).unwrap();
        assert_eq!(dirs.len(), 2);
        assert_eq!(dirs[0], Directive::ImportAs {
            filename: FileName::parse("rfc9052"),
            alias: "cose".to_owned()
        });
        assert_eq!(dirs[1], Directive::IncludeFrom {
            names: vec!["*".to_owned()],
            filename: FileName::parse("rfc8610")
        });
    }

    #[test]
    fn ignores_non_directive_lines() {
        let input = ";# import rfc9052\n; just a comment\n;# include rfc8610\n";
        let dirs = parse_directives(input).unwrap();
        assert_eq!(dirs.len(), 2);
    }

    #[test]
    fn ignores_lines_without_whitespace_after_hash() {
        let dirs = parse_directives(";#import rfc9052").unwrap();
        assert!(dirs.is_empty());
    }

    #[test]
    fn handles_tabs_as_whitespace() {
        let dirs = parse_directives(";#\timport\trfc9052\tas\tcose").unwrap();
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0], Directive::ImportAs {
            filename: FileName::parse("rfc9052"),
            alias: "cose".to_owned()
        });
    }

    #[test]
    fn returns_error_for_unknown_keyword() {
        let err = parse_directives(";# unknown rfc9052").unwrap_err();
        assert_eq!(err.line, 0);
        assert!(matches!(
            err.kind,
            DirectiveParseErrorKind::UnknownKeyword(_)
        ));
    }

    #[test]
    fn returns_error_for_missing_filename() {
        let err = parse_directives(";# import").unwrap_err();
        assert_eq!(err.line, 0);
        assert_eq!(err.kind, DirectiveParseErrorKind::MissingFilename);
    }

    #[test]
    fn returns_error_for_missing_alias() {
        let err = parse_directives(";# import rfc9052 as").unwrap_err();
        assert_eq!(err.line, 0);
        assert_eq!(err.kind, DirectiveParseErrorKind::MissingAlias);
    }
}
