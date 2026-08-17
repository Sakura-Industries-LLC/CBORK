// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: AGPL-3.0-only

//! Integration tests for map-key validation.
//!
//! These exercise the validator's *internal* logic directly: for every
//! committed map-key vector under `cddl/vectors/project/map-key/`, the
//! matching CBOR value must validate against its schema and the
//! negative value must not. The companion `scripts/test-vectors.sh`
//! performs the same checks through the `cbork` binary end-to-end.

#![allow(clippy::expect_used, reason = "Allowed in tests")]

use std::path::{Path, PathBuf};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cddl/vectors/project/map-key")
}

fn schema_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("map-key fixtures directory")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "cddl"))
        .collect();
    files.sort();
    files
}

#[test]
fn map_key_vectors_validate_internally() {
    let dir = fixtures_dir();
    let schemas = schema_files(&dir);
    assert!(!schemas.is_empty(), "no map-key schemas found in {dir:?}");

    for schema in schemas {
        let stem = schema.file_stem().expect("schema stem").to_string_lossy();

        let matching = dir.join(format!("{stem}.cbor"));
        assert!(
            matching.exists(),
            "missing matching vector for {stem}: {}",
            matching.display()
        );
        assert!(
            cbork::validate::exec(&schema, Some(&matching), false, false, false, None, true),
            "{stem}: matching vector must validate: {}",
            schema.display()
        );

        let negative = dir.join(format!("{stem}-negative.cbor"));
        assert!(
            negative.exists(),
            "missing negative vector for {stem}: {}",
            negative.display()
        );
        assert!(
            !cbork::validate::exec(&schema, Some(&negative), false, false, false, None, true),
            "{stem}: negative vector must not validate: {}",
            schema.display()
        );
    }
}
