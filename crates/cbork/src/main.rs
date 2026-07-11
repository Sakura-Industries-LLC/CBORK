// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: AGPL-3.0-only

//! CDDL linter cli tool

mod agent_skills;
mod cli;
mod decode;
mod diagnostics;
mod lint;
mod render;
mod render_abnf_breakdown;
mod rfc;
mod ui;
mod validate;
mod why;
mod xref;

fn main() {
    cli::cli().run().exec();
}
