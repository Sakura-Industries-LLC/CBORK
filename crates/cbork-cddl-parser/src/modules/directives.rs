// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MIT OR Apache-2.0

//! CDDL module directive types.
//!
//! These represent parsed module directives (`;# import ...`, `;# include ...`)
//! from the CDDL Modules draft (draft-ietf-cbor-cddl-modules).

use std::path::Path;

/// A target module filename, classified by how it should be resolved.
///
/// Classification follows the Step 5 naming rules:
/// * Unquoted names (e.g. `rfc9052`) are [`WellKnown`](FileName::WellKnown) and resolved
///   through the compile-time catalog.
/// * Quoted names starting with `"/"` are [`Absolute`](FileName::Absolute).
/// * All other quoted names are [`Relative`](FileName::Relative).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileName {
    /// A built-in standard module name resolved from the catalog
    /// (e.g. `rfc9052`).
    WellKnown(String),
    /// A relative filesystem path (e.g. `"./somedir/file.cddl"`).
    Relative(String),
    /// An absolute filesystem path (e.g. `"/repo/root/file.cddl"`).
    Absolute(String),
}

impl core::fmt::Display for FileName {
    fn fmt(
        &self,
        f: &mut core::fmt::Formatter<'_>,
    ) -> core::fmt::Result {
        match self {
            FileName::WellKnown(name) => write!(f, "{name}"),
            FileName::Relative(path) | FileName::Absolute(path) => {
                write!(f, "\"{path}\"")
            },
        }
    }
}

impl FileName {
    /// Parse a raw filename string (as produced by the directive parser) into
    /// a classified [`FileName`].
    ///
    /// Quoted names (`"..."`) are classified as [`Relative`](FileName::Relative)
    /// or [`Absolute`](FileName::Absolute) based on the inner path prefix.
    /// Unquoted names are [`WellKnown`](FileName::WellKnown).
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        if let Some(inner) = raw.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
            if inner.starts_with('/') {
                FileName::Absolute(inner.to_owned())
            } else {
                FileName::Relative(inner.to_owned())
            }
        } else {
            FileName::WellKnown(raw.to_owned())
        }
    }

    /// Resolve this filename to its CDDL content.
    ///
    /// * [`WellKnown`](FileName::WellKnown) names are looked up in the compile-time
    ///   catalog.
    /// * [`Relative`](FileName::Relative) paths are resolved against `parent_path` (the
    ///   directory of the file containing the directive).
    /// * [`Absolute`](FileName::Absolute) paths are resolved against `root_path` if
    ///   provided, otherwise treated as literal filesystem paths.
    ///
    /// # Errors
    ///
    /// Returns [`FileNameError::NotFound`] if the name is not in the catalog
    /// or the file cannot be read (any IO error is mapped to not-found).
    pub fn resolve(
        &self,
        parent_path: &Path,
        root_path: Option<&Path>,
    ) -> Result<String, FileNameError> {
        match self {
            FileName::WellKnown(name) => {
                cbork_catalog::lookup(name)
                    .map(std::string::ToString::to_string)
                    .ok_or_else(|| FileNameError::NotFound(name.clone()))
            },
            FileName::Relative(path) => {
                let full = parent_path.join(path);
                std::fs::read_to_string(&full).map_err(|_| FileNameError::NotFound(path.clone()))
            },
            FileName::Absolute(path) => {
                let full = if let Some(root) = root_path {
                    root.join(path.trim_start_matches('/'))
                } else {
                    Path::new(path).to_path_buf()
                };
                std::fs::read_to_string(&full).map_err(|_| FileNameError::NotFound(path.clone()))
            },
        }
    }
}

/// Error returned when a [`FileName`] cannot be resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileNameError {
    /// The name was not found in the catalog or the file does not exist /
    /// could not be read.
    NotFound(String),
}

impl core::fmt::Display for FileNameError {
    fn fmt(
        &self,
        f: &mut core::fmt::Formatter<'_>,
    ) -> core::fmt::Result {
        match self {
            FileNameError::NotFound(name) => {
                write!(f, "file not found: {name}")
            },
        }
    }
}

/// A parsed CDDL module directive.
///
/// Directives are encoded as comments in basic CDDL (`;# ...`) and control
/// how rules are imported or included from other modules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Directive {
    /// `;# import <filename>`
    Import {
        /// The target module filename.
        filename: FileName,
    },
    /// `;# import <filename> as <alias>`
    ImportAs {
        /// The target module filename.
        filename: FileName,
        /// The namespace alias applied to imported rules.
        alias: String,
    },
    /// `;# import <name>, ... from <filename>`
    ImportFrom {
        /// The explicitly selected rule names to import.
        names: Vec<String>,
        /// The target module filename.
        filename: FileName,
    },
    /// `;# import <name>, ... from <filename> as <alias>`
    ImportFromAs {
        /// The explicitly selected rule names to import.
        names: Vec<String>,
        /// The target module filename.
        filename: FileName,
        /// The namespace alias applied to imported rules.
        alias: String,
    },
    /// `;# include <filename>`
    Include {
        /// The target module filename.
        filename: FileName,
    },
    /// `;# include <filename> as <alias>`
    IncludeAs {
        /// The target module filename.
        filename: FileName,
        /// The namespace alias applied to included rules.
        alias: String,
    },
    /// `;# include <name>, ... from <filename>`
    IncludeFrom {
        /// The explicitly selected rule names to include.
        names: Vec<String>,
        /// The target module filename.
        filename: FileName,
    },
    /// `;# include <name>, ... from <filename> as <alias>`
    IncludeFromAs {
        /// The explicitly selected rule names to include.
        names: Vec<String>,
        /// The target module filename.
        filename: FileName,
        /// The namespace alias applied to included rules.
        alias: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_parse_well_known() {
        let f = FileName::parse("rfc9052");
        assert_eq!(f, FileName::WellKnown("rfc9052".to_owned()));
    }

    #[test]
    fn filename_parse_relative() {
        let f = FileName::parse("\"./somedir/file.cddl\"");
        assert_eq!(f, FileName::Relative("./somedir/file.cddl".to_owned()));
    }

    #[test]
    fn filename_parse_relative_no_dot_slash() {
        let f = FileName::parse("\"somedir/file.cddl\"");
        assert_eq!(f, FileName::Relative("somedir/file.cddl".to_owned()));
    }

    #[test]
    fn filename_parse_absolute() {
        let f = FileName::parse("\"/absolute/path.cddl\"");
        assert_eq!(f, FileName::Absolute("/absolute/path.cddl".to_owned()));
    }

    #[test]
    fn filename_resolve_well_known() {
        let f = FileName::WellKnown("rfc9052".to_owned());
        let content = f.resolve(Path::new("."), None).unwrap();
        assert!(content.contains("COSE_Key"));
    }

    #[test]
    fn filename_resolve_well_known_not_found() {
        let f = FileName::WellKnown("no-such-module".to_owned());
        let err = f.resolve(Path::new("."), None).unwrap_err();
        assert!(matches!(err, FileNameError::NotFound(_)));
    }

    #[test]
    fn filename_resolve_relative_not_found() {
        let f = FileName::Relative("./no/such/file.cddl".to_owned());
        let err = f.resolve(Path::new("."), None).unwrap_err();
        assert!(matches!(err, FileNameError::NotFound(_)));
    }
}
