// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: AGPL-3.0-only

//! Terminal output helpers for the `cbork` CLI.

use std::path::Path;

use console::style;

/// FIGlet-style `cbork` banner lines printed above command output.
const BANNER_LINES: [&str; 5] = [
    "        __               __",
    "  _____/ /_  ____  _____/ /__",
    " / ___/ __ \\/ __ \\/ ___/ //_/",
    "/ /__/ /_/ / /_/ / /  / ,<",
    "\\___/_.___/\\____/_/  /_/|_|",
];

/// ANSI 256-color ramp used to render the banner from blue to purple.
const BANNER_COLORS: [u8; 5] = [33, 39, 63, 99, 129];

/// Print the startup banner for `cbork`.
pub(crate) fn print_banner() {
    println!();
    for (line, color) in BANNER_LINES.iter().zip(BANNER_COLORS) {
        println!("{}", style(line).color256(color).bold());
    }
    println!(
        "{}",
        style("Modern CDDL tooling for documentation and validation").yellow()
    );
    println!();
}

/// Print a friendly stub message for a command that is parsed but not wired.
pub(crate) fn print_stub(
    command_name: &str,
    path: Option<&Path>,
    summary: &str,
) {
    let target = path.map_or_else(|| "<none>".to_string(), |path| path.display().to_string());
    println!(
        "{}\n{}\n{} {}",
        style(format!("command scaffolded: {command_name}"))
            .yellow()
            .bold(),
        style(summary).dim(),
        style("target:").bold(),
        target
    );
    println!(
        "{}",
        style("This command is parsed but not wired to behavior yet.").dim()
    );
}
