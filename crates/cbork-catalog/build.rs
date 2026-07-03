// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MPL-2.0

//! Build script: generates a compile-time catalog of CDDL standard modules
//! from `cddl/rfc-std/`, mapping module names to their content.

use std::{
    env, fs,
    io::{self, Write},
    path::Path,
};

fn main() -> io::Result<()> {
    let rfc_std = Path::new("../../cddl/rfc-std");
    let out_dir = env::var("OUT_DIR").map_err(io::Error::other)?;
    let dest = Path::new(&out_dir).join("rfc_std_catalog.rs");

    let mut entries: Vec<(String, String)> = Vec::new();

    println!("cargo:rerun-if-changed={}", rfc_std.display());

    for entry in fs::read_dir(rfc_std)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("cddl") {
            println!("cargo:rerun-if-changed={}", path.display());
            let name = path
                .file_stem()
                .and_then(|n| n.to_str())
                .map(String::from)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid filename"))?;
            let content = fs::read_to_string(&path)?;
            entries.push((name, content));
        }
    }

    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut file = fs::File::create(&dest)?;

    // Build the phf map: name → content
    writeln!(file, "#[allow(clippy::unreadable_literal)]")?;
    writeln!(file, "#[allow(clippy::needless_raw_string_hashes)]")?;
    writeln!(
        file,
        "/// Compile-time catalog mapping built-in module names to CDDL content."
    )?;
    writeln!(file, "/// Generated from `cddl/rfc-std/` at build time.")?;
    writeln!(file, "static CATALOG: phf::Map<&str, &str> = ")?;

    let mut builder = phf_codegen::Map::new();
    for (name, content) in &entries {
        builder.entry(name.clone(), format!("r#\"{content}\"#"));
    }
    write!(file, "{}", builder.build())?;
    writeln!(file, ";")?;

    // Generate the known-names list
    writeln!(file)?;
    writeln!(file, "/// All known built-in module names in sorted order.")?;
    writeln!(file, "static KNOWN_NAMES: &[&str] = &[")?;
    for (name, _) in &entries {
        writeln!(file, "    \"{name}\",")?;
    }
    writeln!(file, "];")?;

    Ok(())
}
