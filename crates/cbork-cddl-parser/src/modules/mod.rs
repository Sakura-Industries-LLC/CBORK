// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Module directive parsing for CDDL module structure.
//!
//! Parses `;# import ...` and `;# include ...` directive comments
//! defined in draft-ietf-cbor-cddl-modules.

mod directives;
mod parser;

pub use directives::{Directive, FileName, FileNameError};
pub use parser::{DirectiveParseError, DirectiveParseErrorKind, parse_directives};
