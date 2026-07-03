// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MPL-2.0

//! Integration test: verifies the compile-time phf catalog matches the vendored
//! `cddl/rfc-std/` source tree exactly.
//!
//! * Every entry in the catalog must have a corresponding `.cddl` file with identical
//!   content.
//! * Every `.cddl` file in the source tree must have a catalog entry.

use std::{fs, path::Path};

const RFC_STD_DIR: &str = "../../cddl/rfc-std";

#[test]
fn catalog_entries_match_fs_files() {
    for name in cbork_catalog::known_names() {
        let file_path = Path::new(RFC_STD_DIR).join(format!("{name}.cddl"));
        assert!(
            file_path.exists(),
            "catalog entry '{name}' has no matching file at {}",
            file_path.display()
        );

        let fs_content = fs::read_to_string(&file_path).unwrap_or_else(|e| {
            panic!("failed to read {}: {e}", file_path.display());
        });

        let catalog_content = cbork_catalog::lookup(name).unwrap_or_else(|| {
            panic!("catalog unexpectedly returned None for known entry '{name}'");
        });

        assert_eq!(
            fs_content, catalog_content,
            "content mismatch for '{name}': file content ({fs_content:?}) != catalog content ({catalog_content:?})"
        );
    }
}

#[test]
fn fs_files_all_have_catalog_entries() {
    let dir_entries = fs::read_dir(RFC_STD_DIR).unwrap_or_else(|e| {
        panic!("failed to read catalog source directory {RFC_STD_DIR}: {e}");
    });

    for entry in dir_entries {
        let entry = entry.unwrap_or_else(|e| {
            panic!("failed to read directory entry: {e}");
        });
        let path = entry.path();

        if path.extension().and_then(|e| e.to_str()) != Some("cddl") {
            continue;
        }

        let name = path
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or_else(|| {
                panic!("invalid filename: {}", path.display());
            });

        assert!(
            cbork_catalog::lookup(name).is_some(),
            "file {} has no corresponding catalog entry",
            path.display()
        );
    }
}
