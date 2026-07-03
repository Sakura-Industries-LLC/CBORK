// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: AGPL-3.0-only

//! CDDL linter cli tool

mod cli;
mod decode;
mod diagnostics;
mod lint;
mod render;
mod rfc;
mod ui;
mod validate;
mod why;
mod xref;

fn main() {
    cli::cli().run().exec();
}
