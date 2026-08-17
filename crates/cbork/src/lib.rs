// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: AGPL-3.0-only

//! `cbork` library: CDDL linting, CBOR validation, and effective CDDL
//! rendering.
//!
//! The `cbork` binary is a thin wrapper over this library's
//! [`cli`] entry point. Integration tests link against this crate to
//! exercise the validator directly.

pub mod agent_skills;
pub mod cli;
pub mod decode;
pub mod diagnostics;
pub mod lint;
pub mod render;
pub mod render_abnf_breakdown;
pub mod rfc;
pub mod ui;
pub mod validate;
pub mod why;
pub mod xref;
