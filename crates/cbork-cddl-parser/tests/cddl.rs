// Copyright (c) 2023 Input Output (IOG).
// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MIT OR Apache-2.0

//! CDDL Parser Tests
use std::{ffi::OsStr, fs, io::Result, path::Path};

use cbork_cddl_parser::validate_cddl;

/// Walk a directory of `.cddl` files and validate them.
/// Files prefixed with `valid_` or in a `positive/` directory are expected to parse.
/// Files prefixed with `invalid_` or in a `negative/` directory are expected to fail.
fn check_cddl_dir(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    let is_project_negative = dir.components().any(|c| c.as_os_str() == "project")
        && dir.components().any(|c| c.as_os_str() == "negative");
    if is_project_negative {
        return;
    }

    let mut file_paths: Vec<_> = entries
        .filter_map(Result::ok)
        .filter_map(|x| x.path().is_file().then_some(x.path()))
        .filter(|p| p.extension().and_then(OsStr::to_str) == Some("cddl"))
        .collect();

    file_paths.sort();

    let is_positive_dir = dir
        .file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|n| n == "positive");
    let is_negative_dir = dir
        .file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|n| n == "negative");

    let valid_file_paths: Vec<_> = file_paths
        .iter()
        .filter(|p| {
            let name = p.file_name().and_then(OsStr::to_str).unwrap_or("");
            is_positive_dir || name.starts_with("valid")
        })
        .collect();
    let invalid_file_paths: Vec<_> = file_paths
        .iter()
        .filter(|p| {
            let name = p.file_name().and_then(OsStr::to_str).unwrap_or("");
            is_negative_dir || name.starts_with("invalid")
        })
        .collect();

    // If no valid/invalid convention, assume all are valid.
    let all_valid = valid_file_paths.is_empty() && invalid_file_paths.is_empty();
    let valid_paths: Vec<_> = if all_valid {
        file_paths.iter().collect()
    } else {
        valid_file_paths
    };

    let mut err_messages = vec![];
    for file_path in &valid_paths {
        let Ok(content) = fs::read_to_string(file_path) else {
            let idx = err_messages.len().wrapping_add(1);
            err_messages.push(format!("{idx}) failed to read {}", file_path.display()));
            continue;
        };
        if let Err(e) = validate_cddl(&content) {
            let idx = err_messages.len().wrapping_add(1);
            err_messages.push(format!("{idx}) {} {e}", file_path.display()));
        }
    }

    for file_path in &invalid_file_paths {
        let Ok(content) = fs::read_to_string(file_path) else {
            let idx = err_messages.len().wrapping_add(1);
            err_messages.push(format!("{idx}) failed to read {}", file_path.display()));
            continue;
        };
        let result = validate_cddl(&content);
        assert!(
            result.is_err(),
            "{} is expected to fail",
            file_path.display()
        );
    }

    let err_msg = err_messages.join("\n\n");
    assert!(err_msg.is_empty(), "{err_msg}");
}

#[test]
/// # Panics
fn parse_cddl_files() {
    check_cddl_dir(Path::new("../../cddl/vectors/project/positive"));
    check_cddl_dir(Path::new("../../cddl/vectors/project/negative"));
}

#[test]
/// # Panics
fn parse_rfc_vectors() {
    check_cddl_dir(Path::new("../../cddl/vectors/rfc"));
}

#[test]
/// # Panics
fn parse_rfc_std() {
    // RFC std files are canonical schemas; skip known placeholder-template files.
    let skip = [
        "rfc9594-example-extended-scope.cddl",
        "rfc9594-example-extended-scope-aif.cddl",
        "rfc9594-example-extended-scope-text.cddl",
    ];
    let dir = Path::new("../../cddl/rfc-std");
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    let mut file_paths: Vec<_> = entries
        .filter_map(Result::ok)
        .filter_map(|x| x.path().is_file().then_some(x.path()))
        .filter(|p| {
            p.extension().and_then(OsStr::to_str) == Some("cddl")
                && !skip.contains(&p.file_name().and_then(OsStr::to_str).unwrap_or(""))
        })
        .collect();
    file_paths.sort();

    let mut err_messages = vec![];
    for file_path in file_paths {
        let Ok(content) = fs::read_to_string(&file_path) else {
            let idx = err_messages.len().wrapping_add(1);
            err_messages.push(format!("{idx}) failed to read {}", file_path.display()));
            continue;
        };
        if let Err(e) = validate_cddl(&content) {
            let idx = err_messages.len().wrapping_add(1);
            err_messages.push(format!("{idx}) {} {e}", file_path.display()));
        }
    }
    let err_msg = err_messages.join("\n\n");
    assert!(err_msg.is_empty(), "{err_msg}");
}
