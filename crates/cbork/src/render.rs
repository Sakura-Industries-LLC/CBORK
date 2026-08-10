// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: AGPL-3.0-only

//! `cbork render` subcommand.
//!
//! Renders a compiled CDDL file as a "concrete view" — the effective
//! CDDL the compiler actually reasons about, with named constants
//! folded, socket/group plug augmentations inlined, structural type
//! references inlined, and prunable/redundant rules dropped. The
//! renderer works from `complete_nodes` (the post-include, post-generic,
//! post-prune tree) and a `ResolutionMap` built from the same tree, so
//! what the user sees in the output is what the linter sees.
//!
//! Only the selected root rule and its reachable closure are emitted;
//! unrelated definitions from a source library are folded or dropped.
//!
//! The same machinery in `cbork_cddl_compiler::concrete` drives this
//! command and the diff-style output for `.within` / `.and` checks.

use std::{fmt::Write as _, path::Path};

use cbork_cddl_compiler::{
    CompiledCDDL, ConcretePolicy, ResolutionMap, build_resolution, render_to_string,
};
use console::Emoji;

/// Execute the `cbork render` subcommand.
pub(crate) fn exec(
    path: &Path,
    json: bool,
    no_comments: bool,
) -> bool {
    let compiled = match CompiledCDDL::compile(path, None) {
        Ok(c) => c,
        Err(e) => {
            println!(
                "{} {}:\n{}",
                Emoji::new("🚨", "Compile Error"),
                path.display(),
                e
            );
            return false;
        },
    };

    let policy = build_policy(no_comments);
    let resolution = build_resolution(&compiled.complete_nodes);
    let rendered = render_to_string(&compiled.complete_nodes, &resolution, &policy);

    if json {
        let json = format_json(&rendered);
        println!("{json}");
    } else {
        if !no_comments {
            let header = render_header(path, &compiled, &resolution);
            print!("{header}");
        }
        print!("{rendered}");
        if !rendered.ends_with('\n') {
            println!();
        }
    }

    true
}

/// Build a render policy: always the concrete view, with comments
/// suppressed when requested.
fn build_policy(no_comments: bool) -> ConcretePolicy {
    ConcretePolicy::for_render().with_comments(!no_comments)
}
/// Emit the leading `; cbork render ...` comment block that prefixes
/// the concrete view.
fn render_header(
    path: &Path,
    compiled: &CompiledCDDL,
    resolution: &ResolutionMap,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "; cbork render — concrete view of {}", path.display());
    let _ = writeln!(out, ";   user nodes: {}", compiled.user_nodes.len());
    let _ = writeln!(out, ";   complete nodes: {}", compiled.complete_nodes.len());
    let _ = writeln!(out, ";   resolved types: {}", compiled.resolved_types.len());
    let _ = writeln!(
        out,
        ";   definitions: {}, socket plugs: {}",
        resolution.definitions.len(),
        resolution.socket_plugs.len()
    );
    let _ = writeln!(out, ";   mode: concrete (named constants folded)");
    let _ = writeln!(out, ";");
    out
}

/// Wrap a CDDL string as a JSON string value. Hand-rolled to avoid
/// pulling in `serde_json` for this small surface.
fn format_json(text: &str) -> String {
    let mut out = String::with_capacity(text.len().saturating_add(2));
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            },
            c => out.push(c),
        }
    }
    out.push('"');
    format!("{{\"concrete_cddl\": {out}}}")
}
