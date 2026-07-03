// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MPL-2.0

//! Literal handling utilities.
//!
//! This module owns the concrete literal wrappers used by the compiler.

#![allow(
    clippy::missing_errors_doc,
    clippy::double_must_use,
    clippy::must_use_candidate
)]

/// Literal arrays used by semantic operators like `.join` and `.printf`.
pub mod array;
/// Byte-string literal parsing and encoding helpers.
pub mod byte;
/// XML Schema regular-expression literal parsing and validation helpers.
pub mod regex;
/// Text-string literal parsing, dedenting, and conversion helpers.
pub mod text;
