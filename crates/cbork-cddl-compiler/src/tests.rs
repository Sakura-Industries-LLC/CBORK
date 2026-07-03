// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MPL-2.0

//! Tests for the CDDL compiler — Step 3: AST metadata for pruning and emission control.
//!
//! Each test writes a temporary `.cddl` file to a test directory so that
//! [`CompiledCDDL::compile`] can exercise the real file-I/O path.
//!
//! Note: node counts include the standard postlude appended by the parser,
//! so tests check for the presence and order of user-provided content
//! rather than exact total counts.  Postlude nodes are stored separately
//! in [`CompiledCDDL::postlude_nodes`].

use std::{
    io::Write as _,
    path::{Path, PathBuf},
};

use cbork_cddl_parser::modules::{Directive, FileName};

use crate::{
    MetaData, SourceOrigin, WrappedNode,
    compiled::{CompiledCDDL, dump_tree},
    resolver_cache::{EntryState, ResolverCache},
};

/// Helper: write `content` to a temp file and return the path.
fn write_temp_file(
    name: &str,
    content: &str,
) -> PathBuf {
    let dir = std::env::temp_dir().join("cbork_compiler_test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(content.as_bytes()).unwrap();
    path
}

#[test]
fn compile_simple_no_directives() {
    let path = write_temp_file("simple.cddl", "foo = 1\nbaz = 2\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    let rule_count = compiled
        .user_nodes
        .iter()
        .filter(|n| matches!(n, WrappedNode::RuleLine { .. }))
        .count();
    assert_eq!(rule_count, 2);
    assert!(compiled.warnings.iter().any(|diagnostic| {
        diagnostic.code == "E020"
            && diagnostic.level == crate::DiagnosticLevel::Error
            && diagnostic
                .message
                .contains("unreferenced top-level definition `baz`")
    }));

    // User nodes should not be tagged
    for node in &compiled.user_nodes {
        assert!(
            node.metadata().is_empty(),
            "user nodes should have no metadata"
        );
    }

    // Postlude nodes should all be Silent
    assert!(!compiled.postlude_nodes.is_empty());
    for node in &compiled.postlude_nodes {
        assert!(
            node.metadata().contains(&MetaData::Silent),
            "postlude nodes should be tagged Silent"
        );
    }

    let dump = dump_tree(&compiled);
    assert!(dump.contains("user nodes:"));
    assert!(dump.contains("postlude nodes"));
    assert!(dump.contains("RuleLine: foo = 1"));
    assert!(dump.contains("RuleLine: baz = 2"));
    assert!(
        dump.contains("Syntax["),
        "tree dump should show nested syntax"
    );
    assert!(
        dump.contains("Silent"),
        "tree dump should show Silent metadata on postlude"
    );
}

#[test]
fn library_multiple_top_level_roots_downgrade_to_warning() {
    let path = write_temp_file(
        "library_multiple_roots.cddl",
        ";@ CBORK: Library\nfoo = 1\nbaz = 2\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(compiled.warnings.iter().any(|diagnostic| {
        diagnostic.code == "E020"
            && diagnostic.level == crate::DiagnosticLevel::Warning
            && diagnostic
                .message
                .contains("unreferenced top-level definition `baz`")
    }));
}

#[test]
fn library_directive_downgrades_undefined_references_to_warnings() {
    let path = write_temp_file(
        "library_undefined_refs.cddl",
        ";@ CBORK: Library\nexport = external-type\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(compiled.is_library);
    assert!(compiled.warnings.iter().any(|diagnostic| {
        diagnostic.code == "E016"
            && diagnostic.level == crate::DiagnosticLevel::Warning
            && diagnostic
                .message
                .contains("undefined reference `external-type`")
    }));
    assert!(!compiled.warnings.iter().any(|diagnostic| {
        diagnostic.code == "E016" && diagnostic.level == crate::DiagnosticLevel::Error
    }));
}

#[test]
fn library_directive_must_appear_before_non_comment_content() {
    let path = write_temp_file(
        "library_directive_misplaced.cddl",
        "root = thing\n;@ CBORK: Library\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(compiled.warnings.iter().any(|diagnostic| {
        diagnostic.code == "E018"
            && diagnostic.level == crate::DiagnosticLevel::Error
            && diagnostic
                .message
                .contains("misplaced `;@ CBORK: Library` directive")
    }));
}

#[test]
fn library_directive_must_not_appear_more_than_once() {
    let path = write_temp_file(
        "library_directive_duplicate.cddl",
        ";@ CBORK: Library\n; comment\n;@ CBORK: Library\nroot = thing\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(compiled.warnings.iter().any(|diagnostic| {
        diagnostic.code == "E018"
            && diagnostic.level == crate::DiagnosticLevel::Error
            && diagnostic
                .message
                .contains("duplicate `;@ CBORK: Library` directive")
    }));
}

#[test]
fn extern_directive_requires_library_mode() {
    let path = write_temp_file(
        "extern_requires_library.cddl",
        ";@ CBORK: Extern outside\nroot = outside\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(compiled.warnings.iter().any(|diagnostic| {
        diagnostic.code == "E019"
            && diagnostic.level == crate::DiagnosticLevel::Error
            && diagnostic.message.contains("requires `;@ CBORK: Library`")
    }));
}

#[test]
fn duplicate_extern_declarations_are_errors() {
    let path = write_temp_file(
        "duplicate_extern.cddl",
        ";@ CBORK: Library\n;@ CBORK: Extern one, two\n;@ CBORK: Extern two\nroot = thing\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(compiled.warnings.iter().any(|diagnostic| {
        diagnostic.code == "E019"
            && diagnostic.level == crate::DiagnosticLevel::Error
            && diagnostic
                .message
                .contains("duplicate extern declaration `two`")
    }));
}

#[test]
fn extern_declaration_suppresses_library_undefined_warning() {
    let path = write_temp_file(
        "extern_suppresses_warning.cddl",
        ";@ CBORK: Library\n;@ CBORK: Extern outside\nroot = outside\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(compiled.is_library);
    assert!(compiled.extern_names.contains("outside"));
    assert!(!compiled.warnings.iter().any(|diagnostic| {
        diagnostic.code == "E016" && diagnostic.message.contains("undefined reference `outside`")
    }));
}

#[test]
fn definite_non_socket_extern_is_an_error() {
    let path = write_temp_file(
        "definite_extern_error.cddl",
        ";@ CBORK: Library\n;@ CBORK: Extern local\nlocal = text\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(compiled.warnings.iter().any(|diagnostic| {
        diagnostic.code == "E019"
            && diagnostic.level == crate::DiagnosticLevel::Error
            && diagnostic
                .message
                .contains("extern declaration `local` contradicts")
    }));
}

#[test]
fn definite_socket_extern_is_allowed() {
    let path = write_temp_file(
        "definite_socket_extern_ok.cddl",
        ";@ CBORK: Library\n;@ CBORK: Extern $plug\n$plug /= text\nroot = 1\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        !compiled.warnings.iter().any(|diagnostic| {
            diagnostic.code == "E019" && diagnostic.message.contains("`$plug`")
        })
    );
}

#[test]
fn compile_with_directive_import() {
    let path = write_temp_file("with_import.cddl", ";# import rfc9052\nfoo = bar\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    // The first 3 user nodes are: ModuleStart, Directive, ModuleEnd
    assert!(matches!(
        compiled.user_nodes[0],
        WrappedNode::ModuleStart { .. }
    ));
    assert!(matches!(
        compiled.user_nodes[1],
        WrappedNode::Directive { .. }
    ));
    assert!(matches!(
        compiled.user_nodes[2],
        WrappedNode::ModuleEnd { .. }
    ));
}

#[test]
fn compile_with_import_as() {
    let path = write_temp_file("import_as.cddl", ";# import rfc9052 as cose\nfoo = bar\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    if let WrappedNode::ModuleStart { ref text, .. } = compiled.user_nodes[0] {
        assert!(
            text.contains("import"),
            "expected import marker, got {text}"
        );
    } else {
        panic!("expected ModuleStart");
    }
    if let WrappedNode::Directive { ref directive, .. } = compiled.user_nodes[1] {
        assert_eq!(directive, &Directive::ImportAs {
            filename: FileName::WellKnown("rfc9052".to_owned()),
            alias: "cose".to_owned()
        });
    } else {
        panic!("expected Directive");
    }
    if let WrappedNode::ModuleEnd { ref text, .. } = compiled.user_nodes[2] {
        assert!(text.contains("End Module"), "expected End Module marker");
    } else {
        panic!("expected ModuleEnd");
    }
}

#[test]
fn compile_preserves_non_directive_comments() {
    let path = write_temp_file(
        "with_comments.cddl",
        "; just a regular comment\nfoo = bar\n; another one\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    let mut comment_count = 0usize;
    let mut rule_count = 0usize;
    collect_kind_counts(&compiled.user_nodes, &mut rule_count, &mut comment_count);

    assert_eq!(rule_count, 1, "expected one rule line");
    assert_eq!(comment_count, 2, "expected two preserved comments");
}

#[test]
fn compile_interleaved_directives_and_rules() {
    let dir = std::env::temp_dir().join("cbork_compiler_test");
    std::fs::create_dir_all(&dir).unwrap();
    let other_path = dir.join("other.cddl");
    std::fs::write(&other_path, "y = 2\n").unwrap();
    let path = write_temp_file(
        "interleaved.cddl",
        ";# import rfc9052\nfoo = bar\n;# include \"./other.cddl\"\nbaz = qux\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    // The first directive is top-level; the second one is nested under the
    // preceding rule line in the tree.
    assert!(matches!(
        compiled.user_nodes[0],
        WrappedNode::ModuleStart { .. }
    ));
    assert!(matches!(
        compiled.user_nodes[1],
        WrappedNode::Directive { .. }
    ));
    assert!(matches!(
        compiled.user_nodes[2],
        WrappedNode::ModuleEnd { .. }
    ));
    assert!(matches!(
        compiled.user_nodes[3],
        WrappedNode::RuleLine { .. }
    ));

    let mut directive_count = 0usize;
    let mut module_start_count = 0usize;
    let mut module_end_count = 0usize;
    collect_variant_counts(
        &compiled.user_nodes,
        &mut directive_count,
        &mut module_start_count,
        &mut module_end_count,
    );
    assert_eq!(directive_count, 2);
    assert_eq!(module_start_count, 2);
    assert_eq!(module_end_count, 2);
}

#[test]
fn compile_missing_file() {
    let path = PathBuf::from("/nonexistent/path.cddl");
    let result = CompiledCDDL::compile(&path, None::<&Path>);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("failed to read"));
}

#[test]
fn compile_parse_error() {
    let path = write_temp_file("bad.cddl", "this is not valid CDDL @@@\n");
    let result = CompiledCDDL::compile(&path, None::<&Path>);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("parse error"));
}

#[test]
fn project_negative_vectors_fail_compiler_validation() {
    let dir = Path::new("../../cddl/vectors/project/negative");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|s| s.to_str()) != Some("cddl") {
            continue;
        }

        let result = CompiledCDDL::compile(&path, None::<&Path>);
        assert!(
            result.is_err(),
            "{} is expected to fail compiler validation",
            path.display()
        );
    }
}

#[test]
fn project_positive_vectors_compile_cleanly() {
    // Step 7 baseline: every `.cddl` file under
    // `cddl/vectors/project/positive/` must compile successfully
    // (no fatal resolver errors).  Hard errors such as
    // `E020 unreferenced top-level definition` are tolerated
    // because several positive vectors are deliberately
    // illustrative example documents that contain definitions
    // the pruner flags as unreferenced; the negative/semantic-error
    // directories own the strict no-error coverage.
    //
    // A handful of vectors reference absolute paths
    // (e.g. `import_absolute_repo_root.cddl`) and rely on the
    // repository root being supplied as the second argument.
    // We compute the repo root from `CARGO_MANIFEST_DIR` and
    // pass it through for every vector in the walk so the
    // absolute-path fixtures resolve identically to the
    // individual integration tests.
    let repo_root = fixture_path("");
    let dir = repo_root.join("cddl/vectors/project/positive");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };

    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|s| s.to_str()) != Some("cddl") {
            continue;
        }

        CompiledCDDL::compile(&path, Some(repo_root.as_path())).unwrap_or_else(|e| {
            panic!(
                "{} must compile, got fatal errors: {:#?}",
                path.display(),
                e.diagnostics
            );
        });
    }
}

#[test]
fn project_semantic_error_vectors_compile_with_diagnostics() {
    // Step 7 baseline: every `.cddl` file under
    // `cddl/vectors/project/semantic-errors/` is either a
    // successful compile that carries at least one diagnostic
    // (the canonical case: a "should lint with E0xx or W00x"
    // fixture) or a parse failure (vectors that pre-date a
    // grammar change and were never re-baselined — the parser
    // catches them with E002).  The strict no-parse-failure
    // coverage lives in `project_negative_vectors_fail_compiler_validation`,
    // which owns the `negative/` directory.
    //
    // The important invariant for `semantic-errors/` is that
    // vectors that DO compile must produce at least one
    // diagnostic.  Individual integration tests pin the exact
    // diagnostic code for each fixture; this walk guards
    // against silent regressions where a previously-linting
    // vector stops triggering any diagnostic at all.
    let repo_root = fixture_path("");
    let dir = repo_root.join("cddl/vectors/project/semantic-errors");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };

    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|s| s.to_str()) != Some("cddl") {
            continue;
        }
        // Skip support files.  They are included by the main
        // fixture, not compiled standalone, so they have no
        // diagnostic of their own to assert.  The convention is
        // a `_lib` filename suffix (e.g.
        // `cbork_export_before_import_lib.cddl`) or a file under
        // a `support/` subdirectory.
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if stem.ends_with("_lib") {
            continue;
        }
        if path.components().any(|c| c.as_os_str() == "support") {
            continue;
        }

        match CompiledCDDL::compile(&path, Some(repo_root.as_path())) {
            Ok(compiled) => {
                assert!(
                    !compiled.warnings.is_empty(),
                    "{} compiled but produced no diagnostic",
                    path.display()
                );
            },
            Err(e) => {
                // Parse failures are tolerated here; the negative
                // directory already covers that ground.
                let has_parse_error = e.diagnostics.iter().any(|d| d.code == "E002");
                assert!(
                    has_parse_error,
                    "{} failed to compile with non-parse errors: {:#?}",
                    path.display(),
                    e.diagnostics
                );
            },
        }
    }
}

#[test]
fn repeated_include_of_same_file_fails() {
    let dir = std::env::temp_dir().join("cbork_compiler_test");
    std::fs::create_dir_all(&dir).unwrap();
    let lib_path = dir.join("repeat_source.cddl");
    std::fs::write(&lib_path, "repeat = 1\n").unwrap();
    let path = write_temp_file(
        "repeat_main.cddl",
        ";# include \"./repeat_source.cddl\"\n;# include \"./repeat_source.cddl\"\nroot = top\n",
    );

    let result = CompiledCDDL::compile(&path, None::<&Path>);
    assert!(result.is_err(), "repeated inclusion should fail hard");
}

#[test]
fn repeated_stdlib_import_is_not_a_cycle() {
    // Imports are weak, scope-bound references.  Importing the same
    // well-known module twice from sibling directives is not a cycle
    // even when the alias is the same: each import produces a separate
    // subtree that the duplicate-definition pass will then collapse or
    // flag as redundant.  The resolver must not reject it up front.
    let path = write_temp_file(
        "repeat_stdlib.cddl",
        ";# import rfc9052\n;# import rfc9052\nroot = top\n",
    );

    let result = CompiledCDDL::compile(&path, None::<&Path>);
    assert!(
        result.is_ok(),
        "repeated stdlib import should not be a cycle, got: {:?}",
        result.err().map(|e| e.to_string())
    );
}

#[test]
fn named_import_as_requires_prefixed_selected_names() {
    let dir = std::env::temp_dir().join("cbork_compiler_test");
    std::fs::create_dir_all(&dir).unwrap();
    let lib_path = dir.join("aliased_import_source.cddl");
    std::fs::write(&lib_path, "dog = tstr\n").unwrap();
    let path = write_temp_file(
        "named_import_as_requires_prefix.cddl",
        ";# import dog from \"./aliased_import_source.cddl\" as frog\nroot = frog.dog\n",
    );

    let err = CompiledCDDL::compile(&path, None::<&Path>).unwrap_err();
    assert!(
        err.to_string().contains("must be prefixed with `frog.`"),
        "{err}"
    );
}

#[test]
fn import_as_does_not_prefix_prelude_names() {
    let dir = std::env::temp_dir().join("cbork_compiler_test");
    std::fs::create_dir_all(&dir).unwrap();
    let lib_path = dir.join("argon2id_import_source.cddl");
    std::fs::write(&lib_path, "tag<t> = ( bstr .x-hash t ) .size 32\n").unwrap();
    let path = write_temp_file(
        "import_as_does_not_prefix_prelude.cddl",
        ";# import a2d.tag<t> from \"./argon2id_import_source.cddl\" as a2d\nroot<t> = a2d.tag<t>\n",
    );

    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    assert!(
        !compiled.warnings.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("undefined reference `a2d.bstr`")
        }),
        "{:#?}",
        compiled.warnings
    );
}

#[test]
fn tree_dump_output() {
    // Reference a cose.* type so the imported subtree is reachable and
    // survives the reachability pruner.  Without the reference the
    // imported material would be pruned and the dump would not show
    // the aliased resolved content.
    let path = write_temp_file(
        "dump_test.cddl",
        ";# import rfc9052 as cose\nfoo = cose.COSE_Key\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    let dump = dump_tree(&compiled);

    assert!(dump.contains("CompiledCDDL"));
    assert!(dump.contains("user nodes:"));
    assert!(dump.contains("postlude nodes"));
    assert!(dump.contains("Module: import"));
    assert!(dump.contains("Directive: ImportAs"));
    assert!(dump.contains("End Module"));
    assert!(dump.contains("RuleLine: foo = cose.COSE_Key"));
    assert!(
        dump.contains("Syntax["),
        "tree dump should show nested syntax"
    );
    assert!(
        dump.contains("Silent"),
        "tree dump should show Silent metadata"
    );
    // After resolution, the directive should have children
    assert!(
        dump.contains("cose."),
        "tree dump should show aliased resolved content"
    );
}

#[test]
fn compile_with_root_path() {
    let path = write_temp_file("rooted.cddl", "foo = bar\n");
    let root = std::env::temp_dir().join("cbork_root");
    let compiled = CompiledCDDL::compile(&path, Some(root.as_path())).unwrap();
    assert_eq!(compiled.root_path, Some(root));
}

#[test]
fn display_trait_works() {
    let path = write_temp_file("display_test.cddl", "foo = bar\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    let display_str = compiled.to_string();
    assert!(display_str.contains("RuleLine: foo = bar"));
}

#[test]
fn compile_include_directive() {
    let dir = std::env::temp_dir().join("cbork_compiler_test");
    std::fs::create_dir_all(&dir).unwrap();
    let lib_path = dir.join("lib.cddl");
    std::fs::write(&lib_path, "x = 1\n").unwrap();
    let path = write_temp_file(
        "include_test.cddl",
        ";# include \"./lib.cddl\"\nfoo = bar\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    // First 3 user nodes should be ModuleStart, Directive(Include), ModuleEnd
    assert!(matches!(
        compiled.user_nodes[0],
        WrappedNode::ModuleStart { .. }
    ));
    if let WrappedNode::Directive { ref directive, .. } = compiled.user_nodes[1] {
        assert!(matches!(directive, Directive::Include {
            filename: FileName::Relative(_)
        }));
    } else {
        panic!("expected Directive");
    }
    assert!(matches!(
        compiled.user_nodes[2],
        WrappedNode::ModuleEnd { .. }
    ));
}

#[test]
fn strong_definition_wins_over_weak_imported_definition() {
    let dir = std::env::temp_dir().join("cbork_compiler_test");
    std::fs::create_dir_all(&dir).unwrap();
    let lib_path = dir.join("weak_collision_source.cddl");
    std::fs::write(&lib_path, "start = missing\nhelper = 1\n").unwrap();
    let path = write_temp_file(
        "strong_beats_weak.cddl",
        "start = helper\n;# import \"./weak_collision_source.cddl\"\n",
    );

    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        !compiled
            .warnings
            .iter()
            .any(|d| d.message.contains("conflicting definition of `start`")),
        "{:#?}",
        compiled.warnings
    );
    assert!(
        !compiled
            .warnings
            .iter()
            .any(|d| d.message.contains("undefined reference `missing`")),
        "{:#?}",
        compiled.warnings
    );
    assert!(find_rule_node(&compiled.complete_nodes, "helper = 1").is_some());
    assert!(find_rule_node(&compiled.complete_nodes, "start = missing").is_none());
}

#[test]
fn strong_definition_wins_over_identical_weak_imported_definition_without_redundancy() {
    let dir = std::env::temp_dir().join("cbork_compiler_test");
    std::fs::create_dir_all(&dir).unwrap();
    let lib_path = dir.join("weak_identical_source.cddl");
    std::fs::write(&lib_path, "same = 1\n").unwrap();
    let path = write_temp_file(
        "strong_beats_identical_weak.cddl",
        "same = 1\nroot = same\n;# import \"./weak_identical_source.cddl\"\n",
    );

    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        !compiled
            .warnings
            .iter()
            .any(|d| d.message.contains("redundant definition of `same`")),
        "{:#?}",
        compiled.warnings
    );
}

#[test]
fn weak_identical_imports_are_redundant_not_conflicting() {
    let dir = std::env::temp_dir().join("cbork_compiler_test");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("weak_a.cddl"), "thing = 1\n").unwrap();
    std::fs::write(dir.join("weak_b.cddl"), "thing = 1\n").unwrap();
    let path = write_temp_file(
        "weak_identical_imports.cddl",
        "root = thing\n;# import \"./weak_a.cddl\"\n;# import \"./weak_b.cddl\"\n",
    );

    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        compiled
            .warnings
            .iter()
            .any(|d| d.message.contains("redundant definition of `thing`")),
        "{:#?}",
        compiled.warnings
    );
    assert!(
        !compiled
            .warnings
            .iter()
            .any(|d| d.message.contains("conflicting definition of `thing`")),
        "{:#?}",
        compiled.warnings
    );
}

#[test]
fn weak_conflicting_imports_are_hard_errors() {
    let dir = std::env::temp_dir().join("cbork_compiler_test");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("weak_conflict_a.cddl"), "thing = 1\n").unwrap();
    std::fs::write(dir.join("weak_conflict_b.cddl"), "thing = 2\n").unwrap();
    let path = write_temp_file(
        "weak_conflicting_imports.cddl",
        "root = thing\n;# import \"./weak_conflict_a.cddl\"\n;# import \"./weak_conflict_b.cddl\"\n",
    );

    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        compiled
            .warnings
            .iter()
            .any(|d| d.message.contains("conflicting definition of `thing`")),
        "{:#?}",
        compiled.warnings
    );
}

#[test]
fn unreferenced_weak_imports_are_pruned_silently_no_conflict() {
    // The dntls-cose-encrypt regression: two library files each
    // define a weak rule with the same name.  The importer never
    // references it, so both definitions should be pruned by the
    // reachability pruner BEFORE the collision walker runs.  The
    // prune-first ordering rule means no E014 / no W001 is emitted
    // and the rule is absent from the final tree.
    let dir = std::env::temp_dir().join("cbork_compiler_test");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("prunable_lib_a.cddl"),
        ";@ CBORK: Library\nlibrary = all<bstr>\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("prunable_lib_b.cddl"),
        ";@ CBORK: Library\nlibrary = Cose-Encryption-Headers-a256gcm-hkdf256 /\n          Cose-Encryption-Headers-a256gcm /\n          Null-Headers /\n          Protected-Headers-Empty\n",
    )
    .unwrap();
    let path = write_temp_file(
        "unreferenced_weak_imports.cddl",
        "root = top\n;# import \"./prunable_lib_a.cddl\"\n;# import \"./prunable_lib_b.cddl\"\n",
    );

    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        !compiled
            .warnings
            .iter()
            .any(|d| d.message.contains("library")),
        "expected no library-related warnings, got: {:#?}",
        compiled.warnings
    );
    assert!(
        find_rule_node(&compiled.user_nodes, "library = all<bstr>").is_none(),
        "unreferenced weak `library` from prunable_lib_a should be pruned"
    );
}

#[test]
fn importer_strong_definition_silently_drops_unreferenced_weak_imports() {
    // The "importer-wins" case: a library defines a weak `marker` and
    // the importer defines a strong `marker`.  The strong wins
    // silently (PruneOnly) and no E014 / no W001 is emitted.  The
    // import path is also unreferenced, so it should be pruned
    // before the strength check runs.
    let dir = std::env::temp_dir().join("cbork_compiler_test");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("marker_lib.cddl"), "marker = tstr\n").unwrap();
    let path = write_temp_file(
        "importer_strong_marker.cddl",
        "root = marker\nmarker = int\n;# import \"./marker_lib.cddl\"\n",
    );

    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        !compiled
            .warnings
            .iter()
            .any(|d| d.message.contains("`marker`")),
        "expected no marker-related warnings, got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn side_cache_keeps_undefined_reference_visible_for_postlude_injection() {
    // Side-cache regression: the side cache must surface the names
    // of *all* top-level rules that were visible in the unpruned
    // user tree, not just the ones the seed pass re-encounters in
    // the pruned tree.  Without the re-attachment, downstream
    // consumers (the reference-resolution pass, the `user_definition_names`
    // set used by `handle_reference`, and the postlude merge) would
    // see a smaller cache than the original tree contained.
    //
    // Setup: a library defines two top-level rules,
    // `kept_rule` and `phantom_marker`.  The importer uses a
    // *named* import that selects only `kept_rule`, so
    // `phantom_marker` is unreachable (nothing in the user tree
    // references it) and the reachability pruner drops it.  The
    // user file itself only references `lib.kept_rule`; it never
    // references `lib.phantom_marker` and there is no
    // undefined-reference E016 to suppress.  The point of the
    // test is purely that the side cache records
    // `lib.phantom_marker` *anyway* — the side cache is a record
    // of what the original tree contained, independent of
    // whether anything later references it.
    let dir = std::env::temp_dir().join("cbork_compiler_test");
    std::fs::create_dir_all(&dir).unwrap();
    let lib_path = dir.join("side_cache_lib.cddl");
    std::fs::write(&lib_path, "kept_rule = int\nphantom_marker = kept_rule\n").unwrap();
    let path = write_temp_file(
        "side_cache_user.cddl",
        "root = lib.kept_rule\n;# import lib.kept_rule from \"./side_cache_lib.cddl\" as lib\n",
    );

    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    // The named import selected `kept_rule` only, so `phantom_marker`
    // is unreachable and pruned.  The side cache is what guarantees
    // its existence in the original tree still shows up in the final
    // resolver cache after the seed pass replaces the cache with one
    // built from the pruned tree.
    let phantom_in_user_nodes = compiled
        .user_nodes
        .iter()
        .any(|node| matches!(node, WrappedNode::RuleLine { text, .. } if text.contains("phantom_marker")));
    let phantom_in_complete = compiled
        .complete_nodes
        .iter()
        .any(|node| matches!(node, WrappedNode::RuleLine { text, .. } if text.contains("phantom_marker")));
    // The resolver cache must have *some* record that
    // `phantom_marker` (under its alias) was seen — that's the
    // contract the side cache enforces.
    let cache = &compiled.resolved_types;
    let phantom_seen_in_cache = cache.iter().any(|(name, _)| name == "lib.phantom_marker");
    assert!(
        phantom_seen_in_cache,
        "side cache regression: `phantom_marker` was visible in the \
         original tree but the resolver cache has no record of it; \
         user_nodes={} complete_nodes={} cache_entries={:?}",
        phantom_in_user_nodes,
        phantom_in_complete,
        cache.iter().map(|(n, _)| n.to_owned()).collect::<Vec<_>>(),
    );
    // And the import must still have brought the selected name in.
    let kept_in_cache = cache.iter().any(|(name, _)| name == "lib.kept_rule");
    assert!(
        kept_in_cache,
        "expected the selected `kept_rule` to be present in the cache"
    );
}

#[test]
fn named_import_keeps_full_subtree_for_later_pruning() {
    let dir = std::env::temp_dir().join("cbork_compiler_test");
    std::fs::create_dir_all(&dir).unwrap();
    let lib_path = dir.join("named_import_source.cddl");
    std::fs::write(&lib_path, "keep = 1\ndrop = 2\n").unwrap();
    // Reference the kept rule from a non-prunable root so the kept
    // rule survives the reachability pruner.  The dropped rule is
    // never referenced and is pruned in a single pass.
    let path = write_temp_file(
        "named_import_main.cddl",
        ";# import keep from \"./named_import_source.cddl\"\nroot = keep\n",
    );

    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    let keep = find_rule_node(&compiled.user_nodes, "keep = 1")
        .expect("expected keep rule to remain in tree");
    assert!(
        !keep.metadata().contains(&MetaData::Prunable),
        "selected rule should not be marked prunable"
    );

    // The dropped rule was never reachable; the reachability pruner
    // removed it before it could even be marked Prunable.
    assert!(
        find_rule_node(&compiled.user_nodes, "drop = 2").is_none(),
        "unreachable prunable rule should have been pruned"
    );

    assert!(
        find_rule_node(&compiled.complete_nodes, "keep = 1").is_some(),
        "selected rule should remain in the complete tree"
    );
    assert!(
        find_rule_node(&compiled.complete_nodes, "drop = 2").is_none(),
        "unreachable prunable rule should be removed from the complete tree"
    );
}

#[test]
fn pruning_ignores_dangling_references_in_removed_rules() {
    let dir = std::env::temp_dir().join("cbork_compiler_test");
    std::fs::create_dir_all(&dir).unwrap();
    let lib_path = dir.join("pruned_dangling_source.cddl");
    std::fs::write(&lib_path, "selected = 1\nunused = missing\n").unwrap();
    let path = write_temp_file(
        "pruned_dangling_main.cddl",
        "root = selected\n;# import selected from \"./pruned_dangling_source.cddl\"\n",
    );

    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        find_rule_node(&compiled.complete_nodes, "selected = 1").is_some(),
        "selected imported rule should remain"
    );
    assert!(
        find_rule_node(&compiled.complete_nodes, "unused = missing").is_none(),
        "unused prunable rule should be removed"
    );
    assert!(
        !compiled
            .warnings
            .iter()
            .any(|diagnostic| diagnostic.message.contains("undefined reference `missing`")),
        "dangling references inside pruned rules should not be reported"
    );
}

#[test]
fn pruning_retains_reachable_prunable_rules_and_reports_their_dangling_refs() {
    let dir = std::env::temp_dir().join("cbork_compiler_test");
    std::fs::create_dir_all(&dir).unwrap();
    let lib_path = dir.join("retained_dangling_source.cddl");
    std::fs::write(&lib_path, "helper = missing\n").unwrap();
    let path = write_temp_file(
        "retained_dangling_main.cddl",
        "root = helper\n;# import \"./retained_dangling_source.cddl\"\n",
    );

    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        find_rule_node(&compiled.complete_nodes, "helper = missing").is_some(),
        "reachable prunable rule should remain"
    );
    assert!(
        compiled
            .warnings
            .iter()
            .any(|diagnostic| diagnostic.message.contains("undefined reference `missing`")),
        "dangling references inside retained rules should be reported"
    );
}

#[test]
fn comment_with_multiple_directives() {
    let dir = std::env::temp_dir().join("cbork_compiler_test");
    std::fs::create_dir_all(&dir).unwrap();
    let lib_path = dir.join("lib.cddl");
    std::fs::write(&lib_path, "x = 1\n").unwrap();
    let path = write_temp_file(
        "multi_dir.cddl",
        "foo = bar\n;# import rfc9052\n;# include \"./lib.cddl\"\nbaz = qux\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(matches!(
        compiled.user_nodes[0],
        WrappedNode::RuleLine { .. }
    ));

    let mut directive_count = 0usize;
    let mut module_start_count = 0usize;
    let mut module_end_count = 0usize;
    collect_variant_counts(
        &compiled.user_nodes,
        &mut directive_count,
        &mut module_start_count,
        &mut module_end_count,
    );
    assert_eq!(directive_count, 2);
    assert_eq!(module_start_count, 2);
    assert_eq!(module_end_count, 2);
}

#[test]
fn compiledcddl_node_kind_labels() {
    let path = write_temp_file(
        "labels.cddl",
        "; plain comment\nfoo = bar\n;# import rfc9052\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    let mut labels = Vec::new();
    collect_kind_labels(&compiled.user_nodes, &mut labels);

    assert!(labels.contains(&"Comment"));
    assert!(labels.contains(&"RuleLine"));
    assert!(labels.contains(&"ModuleStart"));
    assert!(labels.contains(&"Directive"));
    assert!(labels.contains(&"ModuleEnd"));
}

#[test]
fn user_nodes_have_no_metadata_by_default() {
    let path = write_temp_file("clean.cddl", "foo = bar\n; just a note\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    for node in &compiled.user_nodes {
        let meta = node.metadata();
        assert!(
            meta.is_empty(),
            "user node {} should have no metadata, got {meta:?}",
            node.kind_label()
        );
    }
}

#[test]
fn postlude_nodes_are_silent() {
    let path = write_temp_file("ple.cddl", "x = y\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        !compiled.postlude_nodes.is_empty(),
        "postlude should not be empty"
    );
    for node in &compiled.postlude_nodes {
        assert!(
            node.metadata().contains(&MetaData::Silent),
            "postlude node {} should be tagged Silent",
            node.kind_label()
        );
    }
}

#[test]
fn metadata_empty_by_default() {
    // A node freshly constructed without explicit metadata should be empty.
    let node = WrappedNode::Comment {
        text: "; hi".to_owned(),
        span: 0..4,
        origin: SourceOrigin::new(PathBuf::from("test.cddl"), 1, 1),
        metadata: Vec::new(),
    };
    assert!(node.metadata().is_empty());
}

#[test]
fn metadata_prunable_flag() {
    let node = WrappedNode::RuleLine {
        text: "x = 1".to_owned(),
        span: 0..5,
        origin: SourceOrigin::new(PathBuf::from("test.cddl"), 1, 1),
        children: Vec::new(),
        metadata: vec![MetaData::Prunable],
    };
    assert!(node.metadata().contains(&MetaData::Prunable));
    assert!(!node.metadata().contains(&MetaData::Silent));
}

#[test]
fn metadata_multiple_flags() {
    let node = WrappedNode::Syntax {
        rule: "type".to_owned(),
        text: "int".to_owned(),
        span: 0..3,
        origin: SourceOrigin::new(PathBuf::from("test.cddl"), 1, 1),
        children: Vec::new(),
        metadata: vec![MetaData::Prunable, MetaData::Silent],
    };
    assert_eq!(node.metadata().len(), 2);
    assert!(node.metadata().contains(&MetaData::Prunable));
    assert!(node.metadata().contains(&MetaData::Silent));
}

#[test]
fn duplicate_constant_definition_is_tagged_redundant() {
    let path = write_temp_file("redundant_defs.cddl", "a = 42\na = 42\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    // The first definition is kept; the second (redundant) one is
    // pruned and reported via a diagnostic.
    assert!(
        compiled.user_nodes.iter().any(
            |node| matches!(node, WrappedNode::RuleLine { text, .. } if text.contains("a = 42"))
        ),
        "first definition should remain in the tree"
    );
    assert!(
        compiled
            .warnings
            .iter()
            .any(|diagnostic| { diagnostic.message.contains("redundant definition of `a`") }),
        "expected redundant definition diagnostic"
    );
}

#[test]
fn conflicting_constant_definition_is_tagged_conflicting() {
    let path = write_temp_file("conflicting_defs.cddl", "a = 42\na = 57\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    // The first definition is kept; the second (conflicting) one is
    // pruned and reported via a diagnostic.
    assert!(
        compiled.user_nodes.iter().any(
            |node| matches!(node, WrappedNode::RuleLine { text, .. } if text.contains("a = 42"))
        ),
        "first definition should remain in the tree"
    );
    assert!(
        compiled
            .warnings
            .iter()
            .any(|diagnostic| { diagnostic.message.contains("conflicting definition of `a`") }),
        "expected conflicting definition diagnostic"
    );
}

#[test]
fn cache_records_first_definition_origin() {
    let path = write_temp_file("origin_defs.cddl", "a = 42\na = 42\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    let origin = compiled
        .resolved_types
        .origin("a")
        .expect("expected cache origin for resolved definition");

    assert_eq!(origin.source_path, path);
    assert_eq!(origin.line, 1);
    assert_eq!(origin.column, 1);
}

#[test]
fn fixed_point_revisit_does_not_report_self_redundancy() {
    let path = write_temp_file("self_revisit.cddl", "a = 42\nb = 57\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        !compiled
            .warnings
            .iter()
            .any(|d| d.message.contains("redundant definition")),
        "revisiting the same defining rule during fixed-point evaluation must not emit self-redundancy warnings"
    );
}

#[test]
fn conflict_warning_includes_both_locations() {
    let path = write_temp_file("conflict_report.cddl", "a = 42\na = 57\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    let diag = compiled
        .warnings
        .iter()
        .find(|d| d.message.contains("conflicting definition of `a`"))
        .expect("expected conflict diagnostic");

    assert_eq!(diag.source_file.as_ref(), Some(&path));
    assert!(diag.span.is_some(), "diagnostic should carry a span");
    let previous = diag
        .previous_origin
        .as_ref()
        .expect("expected previous origin");
    assert!(
        diag.message.contains("previous definition at"),
        "diagnostic should mention previous definition site"
    );
    assert!(
        diag.message.contains(":1:1") && diag.message.contains(":2:1"),
        "diagnostic should mention both definition locations: {}",
        diag.message
    );
    assert_eq!(previous.source_path, path);
    assert_eq!(previous.line, 1);
    assert_eq!(previous.column, 1);
}

#[test]
fn redundant_warning_includes_both_locations() {
    let path = write_temp_file("redundant_report.cddl", "a = 42\na = 42\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    let diag = compiled
        .warnings
        .iter()
        .find(|d| d.message.contains("redundant definition of `a`"))
        .expect("expected redundant diagnostic");

    assert_eq!(diag.source_file.as_ref(), Some(&path));
    assert!(diag.span.is_some(), "diagnostic should carry a span");
    let previous = diag
        .previous_origin
        .as_ref()
        .expect("expected previous origin");
    assert!(
        diag.message.contains("first defined at"),
        "diagnostic should mention first definition site"
    );
    assert!(
        diag.message.contains(":1:1") && diag.message.contains(":2:1"),
        "diagnostic should mention both definition locations: {}",
        diag.message
    );
    assert_eq!(previous.source_path, path);
    assert_eq!(previous.line, 1);
    assert_eq!(previous.column, 1);
}

#[test]
fn unprefixed_include_silently_drops_against_stronger_local_definition() {
    // A local `=` definition is stronger than an included `=`.  The
    // local definition is kept; the included one is dropped and a
    // hard-error conflict diagnostic is emitted.  This mirrors the
    // spec-strict "include is strong" semantics and is the same
    // behaviour the original `detect_user_definition_collisions`
    // produced, just with the conflicting rule pruned instead of kept
    // in the tree.
    let dir = std::env::temp_dir().join("cbork_compiler_test");
    std::fs::create_dir_all(&dir).unwrap();
    let lib_path = dir.join("dup_source.cddl");
    std::fs::write(&lib_path, "a = 57\n").unwrap();

    let path = write_temp_file(
        "dup_main.cddl",
        "a = 42\n;# include \"./dup_source.cddl\"\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    // Local strong definition is kept.
    assert!(
        find_rule_node(&compiled.user_nodes, "a = 42").is_some(),
        "local strong definition should remain in the tree"
    );
    // A conflicting-definition diagnostic is emitted.
    assert!(
        compiled
            .warnings
            .iter()
            .any(|d| d.message.contains("conflicting definition of `a`")),
        "expected conflicting-definition diagnostic, got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn prefixed_include_does_not_conflict_on_duplicate_definition() {
    let dir = std::env::temp_dir().join("cbork_compiler_test");
    std::fs::create_dir_all(&dir).unwrap();
    let lib_path = dir.join("prefix_source.cddl");
    std::fs::write(&lib_path, "a = 57\n").unwrap();

    let path = write_temp_file(
        "prefix_main.cddl",
        "a = 42\n;# include \"./prefix_source.cddl\" as nested\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    let nested_rule = find_rule_node(&compiled.user_nodes, "nested.a = 57")
        .expect("expected prefixed include rule to remain in tree");
    assert!(
        !nested_rule
            .metadata()
            .contains(&MetaData::ConflictingDefinition),
        "prefixed include should not conflict with the unprefixed name"
    );
    assert!(
        !nested_rule
            .metadata()
            .contains(&MetaData::RedundantDefinition),
        "prefixed include should not be marked redundant either"
    );
}

fn collect_kind_labels<'a>(
    nodes: &'a [WrappedNode],
    labels: &mut Vec<&'a str>,
) {
    for node in nodes {
        labels.push(node.kind_label());
        match node {
            WrappedNode::RuleLine { children, .. }
            | WrappedNode::Syntax { children, .. }
            | WrappedNode::Directive { children, .. } => {
                collect_kind_labels(children, labels);
            },
            WrappedNode::Comment { .. }
            | WrappedNode::ModuleStart { .. }
            | WrappedNode::ModuleEnd { .. } => {},
        }
    }
}

fn collect_kind_counts(
    nodes: &[WrappedNode],
    rule_count: &mut usize,
    comment_count: &mut usize,
) {
    for node in nodes {
        match node {
            WrappedNode::RuleLine { children, .. } => {
                *rule_count = rule_count.wrapping_add(1);
                collect_kind_counts(children, rule_count, comment_count);
            },
            WrappedNode::Comment { .. } => {
                *comment_count = comment_count.wrapping_add(1);
            },
            WrappedNode::Syntax { children, .. } | WrappedNode::Directive { children, .. } => {
                collect_kind_counts(children, rule_count, comment_count);
            },
            WrappedNode::ModuleStart { .. } | WrappedNode::ModuleEnd { .. } => {},
        }
    }
}

fn collect_variant_counts(
    nodes: &[WrappedNode],
    directive_count: &mut usize,
    module_start_count: &mut usize,
    module_end_count: &mut usize,
) {
    for node in nodes {
        match node {
            WrappedNode::Directive { children, .. } => {
                *directive_count = directive_count.wrapping_add(1);
                collect_variant_counts(
                    children,
                    directive_count,
                    module_start_count,
                    module_end_count,
                );
            },
            WrappedNode::ModuleStart { .. } => {
                *module_start_count = module_start_count.wrapping_add(1);
            },
            WrappedNode::ModuleEnd { .. } => {
                *module_end_count = module_end_count.wrapping_add(1);
            },
            WrappedNode::RuleLine { children, .. } | WrappedNode::Syntax { children, .. } => {
                collect_variant_counts(
                    children,
                    directive_count,
                    module_start_count,
                    module_end_count,
                );
            },
            WrappedNode::Comment { .. } => {},
        }
    }
}

#[test]
fn ctlop_int_plus_int() {
    let path = write_temp_file("ctlop_int_plus_int.cddl", "a = 1\nb = 2\nc = a .plus b\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    assert!(compiled.resolved_types.is_resolved("c"));
    let c_val = cache_entry(&compiled.resolved_types, "c");
    assert_eq!(c_val, EntryState::Integer(3));
}

#[test]
fn ctlop_int_plus_float() {
    let path = write_temp_file(
        "ctlop_int_plus_float.cddl",
        "a = 1\nb = 2.5\nc = a .plus b\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    assert!(compiled.resolved_types.is_resolved("c"));
    let c_val = cache_entry(&compiled.resolved_types, "c");
    assert!(matches!(c_val, EntryState::Float(f) if (f - 3.5).abs() < f64::EPSILON));
}

#[test]
fn ctlop_float_plus_int() {
    let path = write_temp_file(
        "ctlop_float_plus_int.cddl",
        "a = 1.5\nb = 2\nc = a .plus b\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    assert!(compiled.resolved_types.is_resolved("c"));
    let c_val = cache_entry(&compiled.resolved_types, "c");
    assert!(matches!(c_val, EntryState::Float(f) if (f - 3.5).abs() < f64::EPSILON));
}

#[test]
fn ctlop_float_plus_float() {
    let path = write_temp_file(
        "ctlop_float_plus_float.cddl",
        "a = 1.5\nb = 2.5\nc = a .plus b\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    assert!(compiled.resolved_types.is_resolved("c"));
    let c_val = cache_entry(&compiled.resolved_types, "c");
    assert!(matches!(c_val, EntryState::Float(f) if (f - 4.0).abs() < f64::EPSILON));
}

// ---------------------------------------------------------------------------
// Step 5.13 — parser-accepted literal constant seeding
// ---------------------------------------------------------------------------

#[test]
fn seed_decimal_integer_resolves() {
    let path = write_temp_file("seed_decimal.cddl", "x = 42\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    assert!(compiled.resolved_types.is_resolved("x"));
    assert_eq!(
        cache_entry(&compiled.resolved_types, "x"),
        EntryState::Integer(42)
    );
}

#[test]
fn seed_decimal_zero_resolves() {
    let path = write_temp_file("seed_zero.cddl", "x = 0\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    assert!(compiled.resolved_types.is_resolved("x"));
    assert_eq!(
        cache_entry(&compiled.resolved_types, "x"),
        EntryState::Integer(0)
    );
}

#[test]
fn seed_hex_integer_resolves() {
    let path = write_temp_file("seed_hex.cddl", "x = 0x10\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    assert!(compiled.resolved_types.is_resolved("x"));
    assert_eq!(
        cache_entry(&compiled.resolved_types, "x"),
        EntryState::Integer(16)
    );
}

#[test]
fn seed_hex_integer_lowercase_resolves() {
    let path = write_temp_file("seed_hex_lc.cddl", "x = 0xdeadbeef\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    assert!(compiled.resolved_types.is_resolved("x"));
    assert_eq!(
        cache_entry(&compiled.resolved_types, "x"),
        EntryState::Integer(0xDEAD_BEEF)
    );
}

#[test]
fn seed_binary_integer_resolves() {
    let path = write_temp_file("seed_binary.cddl", "x = 0b1010\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    assert!(compiled.resolved_types.is_resolved("x"));
    assert_eq!(
        cache_entry(&compiled.resolved_types, "x"),
        EntryState::Integer(10)
    );
}

#[test]
fn seed_negative_decimal_integer_resolves() {
    let path = write_temp_file("seed_neg_dec.cddl", "x = -42\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    assert!(compiled.resolved_types.is_resolved("x"));
    assert_eq!(
        cache_entry(&compiled.resolved_types, "x"),
        EntryState::Integer(-42)
    );
}

#[test]
fn seed_negative_hex_integer_resolves() {
    let path = write_temp_file("seed_neg_hex.cddl", "x = -0x10\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    assert!(compiled.resolved_types.is_resolved("x"));
    assert_eq!(
        cache_entry(&compiled.resolved_types, "x"),
        EntryState::Integer(-16)
    );
}

#[test]
fn seed_negative_binary_integer_resolves() {
    let path = write_temp_file("seed_neg_bin.cddl", "x = -0b1010\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    assert!(compiled.resolved_types.is_resolved("x"));
    assert_eq!(
        cache_entry(&compiled.resolved_types, "x"),
        EntryState::Integer(-10)
    );
}

#[test]
fn seed_decimal_float_resolves() {
    let path = write_temp_file("seed_dec_float.cddl", "x = 3.5\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    assert!(compiled.resolved_types.is_resolved("x"));
    assert!(matches!(cache_entry(&compiled.resolved_types, "x"),
        EntryState::Float(f) if (f - 3.5).abs() < f64::EPSILON));
}

#[test]
fn seed_decimal_scientific_float_resolves() {
    let path = write_temp_file("seed_dec_sci.cddl", "x = 1.5e2\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    assert!(compiled.resolved_types.is_resolved("x"));
    assert!(matches!(cache_entry(&compiled.resolved_types, "x"),
        EntryState::Float(f) if (f - 150.0).abs() < 1e-9));
}

#[test]
fn seed_hexfloat_resolves() {
    // 0x1.fp+2 = (1 + 15/16) * 2^2 = 1.9375 * 4 = 7.75
    let path = write_temp_file("seed_hexfloat.cddl", "x = 0x1.fp+2\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    assert!(compiled.resolved_types.is_resolved("x"));
    assert!(matches!(cache_entry(&compiled.resolved_types, "x"),
        EntryState::Float(f) if (f - 7.75).abs() < 1e-9));
}

#[test]
fn seed_hexfloat_negative_resolves() {
    // -0x1.fp+2 = -7.75
    let path = write_temp_file("seed_hexfloat_neg.cddl", "x = -0x1.fp+2\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    assert!(compiled.resolved_types.is_resolved("x"));
    assert!(matches!(cache_entry(&compiled.resolved_types, "x"),
        EntryState::Float(f) if (f + 7.75).abs() < 1e-9));
}

#[test]
fn seed_hexfloat_no_fraction_resolves() {
    // 0x10p+1 = 16 * 2 = 32
    let path = write_temp_file("seed_hexfloat_nof.cddl", "x = 0x10p+1\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    assert!(compiled.resolved_types.is_resolved("x"));
    assert!(matches!(cache_entry(&compiled.resolved_types, "x"),
        EntryState::Float(f) if (f - 32.0).abs() < 1e-9));
}

#[test]
fn seed_text_literal_resolves() {
    let path = write_temp_file("seed_text.cddl", "x = \"hello\"\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    assert!(compiled.resolved_types.is_resolved("x"));
    let val = cache_entry(&compiled.resolved_types, "x");
    assert!(matches!(val, EntryState::Text(_)));
}

#[test]
fn seed_bytes_literal_resolves() {
    let path = write_temp_file("seed_bytes.cddl", "x = h'abcd'\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    assert!(compiled.resolved_types.is_resolved("x"));
    let val = cache_entry(&compiled.resolved_types, "x");
    assert!(matches!(val, EntryState::Bytes(_)));
}

#[test]
fn seed_one_step_reference_propagates() {
    let path = write_temp_file("seed_one_step.cddl", "a = 0x10\nb = a\nc = b .plus 1\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    assert!(compiled.resolved_types.is_resolved("a"));
    assert!(compiled.resolved_types.is_resolved("b"));
    assert!(compiled.resolved_types.is_resolved("c"));
    assert_eq!(
        cache_entry(&compiled.resolved_types, "a"),
        EntryState::Integer(16)
    );
    assert_eq!(
        cache_entry(&compiled.resolved_types, "b"),
        EntryState::Integer(16)
    );
    assert_eq!(
        cache_entry(&compiled.resolved_types, "c"),
        EntryState::Integer(17)
    );
}

#[test]
fn seed_complex_rhs_remains_unresolved() {
    // A choice is structurally complex and must not be treated
    // as a direct constant, even when every alternative is itself
    // a constant.
    let path = write_temp_file("seed_complex.cddl", "a = 1 / 2\nb = a\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    assert!(
        !compiled.resolved_types.is_resolved("a"),
        "choice RHS must not be seeded as a constant; got: {:#?}",
        compiled.resolved_types
    );
    assert!(
        !compiled.resolved_types.is_resolved("b"),
        "alias of an unresolved definition must not be resolved; got: {:#?}",
        compiled.resolved_types
    );
}

#[test]
fn seed_array_rhs_remains_unresolved() {
    let path = write_temp_file("seed_array.cddl", "a = [1, 2, 3]\nb = a\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    assert!(!compiled.resolved_types.is_resolved("a"));
    assert!(!compiled.resolved_types.is_resolved("b"));
}

#[test]
fn seed_map_rhs_remains_unresolved() {
    let path = write_temp_file("seed_map.cddl", "a = { \"k\" => 1 }\nb = a\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    assert!(!compiled.resolved_types.is_resolved("a"));
    assert!(!compiled.resolved_types.is_resolved("b"));
}

#[test]
fn seed_ctlop_rhs_remains_unresolved() {
    // A control operator on the RHS is a ctlop, not a direct
    // constant.  The seed pass must leave it (and anything that
    // aliases it) unresolved so a later semantic pass can handle
    // it.
    let path = write_temp_file("seed_ctlop.cddl", "a = uint .gt 0\nb = a\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    assert!(!compiled.resolved_types.is_resolved("a"));
    assert!(!compiled.resolved_types.is_resolved("b"));
}

#[test]
fn ctlop_base10_accepts_builtin_integer_rhs() {
    let path = write_temp_file("ctlop_base10_uint.cddl", "schema = text .base10 uint\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(compiled.warnings.is_empty(), "{:#?}", compiled.warnings);
    let rule = find_rule_node(&compiled.complete_nodes, "schema = text .base10")
        .expect("expected the .base10 rule to remain in the tree");
    assert!(
        !rule.metadata().contains(&MetaData::CtlopTypeMismatch),
        "builtin integer RHS should be accepted for .base10"
    );
    assert!(
        !compiled.resolved_types.is_resolved("schema"),
        "indefinite .base10 schemas should validate without folding"
    );
}

#[test]
fn ctlop_base10_accepts_integer_range_rhs() {
    let path = write_temp_file(
        "ctlop_base10_range.cddl",
        "schema = text .base10 byte\nbyte = 0..255\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(compiled.warnings.is_empty(), "{:#?}", compiled.warnings);
    let rule = find_rule_node(&compiled.complete_nodes, "schema = text .base10")
        .expect("expected the .base10 rule to remain in the tree");
    assert!(
        !rule.metadata().contains(&MetaData::CtlopTypeMismatch),
        "integer ranges should be accepted for .base10"
    );
    assert!(
        !compiled.resolved_types.is_resolved("schema"),
        "indefinite .base10 schemas should validate without folding"
    );
}

#[test]
fn ctlop_base10_rejects_float_range_rhs() {
    let path = write_temp_file(
        "ctlop_base10_float_range.cddl",
        "schema = text .base10 fraction\nfraction = 0.0..1.0\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    let rule = find_rule_node(&compiled.complete_nodes, "schema = text .base10")
        .expect("expected the failing .base10 rule to remain in the tree");

    assert!(
        rule.metadata().contains(&MetaData::CtlopTypeMismatch),
        "floating-point ranges should still be rejected for .base10"
    );
    assert!(
        !compiled.resolved_types.is_resolved("schema"),
        "invalid .base10 definitions should not resolve"
    );
}

#[test]
fn ctlop_chain() {
    let path = write_temp_file("ctlop_chain.cddl", "a = 1\nb = a .plus 2\nc = b .plus 3\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    assert_eq!(
        cache_entry(&compiled.resolved_types, "b"),
        EntryState::Integer(3)
    );
    assert_eq!(
        cache_entry(&compiled.resolved_types, "c"),
        EntryState::Integer(6)
    );
}

#[test]
fn generic_expansion_runs_before_ctlop_folding() {
    let path = write_temp_file(
        "generic_before_ctlop.cddl",
        "add-one<t> = t .plus 1\nbase = 2\nthis = add-one<base>\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(compiled.resolved_types.is_resolved("this"));
    assert_eq!(
        cache_entry(&compiled.resolved_types, "this"),
        EntryState::Integer(3)
    );
    assert!(
        !compiled
            .warnings
            .iter()
            .any(|diagnostic| diagnostic.message.contains("undefined reference `t`")),
        "generic parameter should not be reported as a dangling reference"
    );
}

/// Resolve a path under the workspace root from a test fixture
/// fragment.  Tests live in this crate but the fixtures live in the
/// top-level `cddl/vectors/...` tree, so we walk up from
/// `CARGO_MANIFEST_DIR` to the workspace root and join the fragment.
fn fixture_path(rel: &str) -> std::path::PathBuf {
    #[allow(clippy::expect_used, reason = "Allowed in tests")]
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .join(rel)
}

#[test]
fn generic_within_definition_site_negative_emits_e030_at_call_site() {
    // Step 5.7 negative regression: instantiating an imported generic
    // with a concrete argument that violates the definition-site RHS
    // must produce E030 at the consumer call site, and the rendered
    // effective RHS must be the concrete resolved definition rather
    // than an unresolved `std.Wrapper` / `lib.std.Wrapper` alias.
    let path = fixture_path(
        "cddl/vectors/project/semantic-errors/generic_within_definition_site_negative.cddl",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    let diagnostic = compiled
        .warnings
        .iter()
        .find(|diagnostic| diagnostic.code == "E030")
        .expect("instantiated generic `.within` failure must produce E030");
    let span = diagnostic
        .span
        .as_ref()
        .expect("diagnostic should carry a call-site span");
    let source = std::fs::read_to_string(&path).unwrap();
    let template_offset = source
        .find("lib.wrapper<uint>")
        .expect("test fixture should contain call site");
    assert!(
        span.start >= template_offset,
        "E030 should point at the consumer instantiation span, got {diagnostic:#?}"
    );
    let rendered = format!("{diagnostic:#?}");
    // BUG-005 follow-on: the effective mode renderer now produces
    // multiline output, so the RHS may show up as `[\\n  tstr\\n]`
    // in the Debug format rather than `[tstr]` on one line.  Both
    // forms satisfy the contract — the effective RHS shows `tstr`,
    // not an unresolved alias.
    assert!(
        rendered.contains("tstr"),
        "rendered effective RHS must be the concrete resolved `tstr`, not the unresolved alias: {rendered}"
    );
    assert!(
        !rendered.contains("unresolved name"),
        "negative fixture must not surface a generic `unresolved name` regression: {rendered}"
    );
}

#[test]
fn generic_within_definition_site_scope_lints_cleanly() {
    // Step 5.7 positive regression: the imported generic's
    // definition-site alias `std.Wrapper` must resolve through the
    // generic's own scope rather than the consumer's.  The consumer
    // never imports `std` itself.
    let path =
        fixture_path("cddl/vectors/project/positive/generic_within_definition_site_scope.cddl");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        !compiled
            .warnings
            .iter()
            .any(|d| d.code == "E030" && d.message.contains("unresolved name")),
        "definition-site `.within` RHS must resolve; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn generic_within_parameter_substitution_lints_cleanly() {
    // Step 5.7 positive regression: when an imported generic uses
    // bare generic parameters as array entries, the expanded
    // effective LHS must show the concrete arguments — never the
    // unresolved formal parameter names.
    let path =
        fixture_path("cddl/vectors/project/positive/generic_within_parameter_substitution.cddl");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        !compiled
            .warnings
            .iter()
            .any(|d| d.code == "E030" && d.message.contains("unresolved name")),
        "bare generic parameter entries must be substituted with concrete arguments; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn generic_import_retains_private_helper_closure_lints_cleanly() {
    // Step 5.7 regression: when the consumer only cherry-picks a
    // generic from an imported library, the imported generic's
    // private same-module helpers referenced from the generic body
    // must remain reachable after the consumer's alias wrap.
    // RFC 9393 (`cddl/rfc-std/rfc9393-tags.cddl`) is the live
    // regression case.
    let path = fixture_path(
        "cddl/vectors/project/positive/generic_import_retains_private_helper_closure.cddl",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        !compiled.warnings.iter().any(|d| d.code == "E016"),
        "private helper closure must remain reachable without consumer-side imports; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn plain_vs_generic_collision_unreferenced_generic_lints_cleanly() {
    // Step 5.8 regression: when the consumer only cherry-picks a
    // generic from an imported library but never instantiates it,
    // the generic helper is unreferenced weak material and must be
    // pruned silently by the reachability pass.  The Step-5.8
    // collision detector then sees an empty generic set on the LHS
    // of the collision check and emits no E013.
    let path = fixture_path(
        "cddl/vectors/project/positive/plain_vs_generic_collision_unreferenced_generic.cddl",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        !compiled.warnings.iter().any(|d| d.code == "E013"),
        "unreferenced weak imported generic helper must not collide with a strong local plain rule; \
         got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn plain_vs_generic_collision_unreferenced_unaliased_generic_lints_cleanly() {
    // Step 5.8 unaliased regression: the cherry-picked generic shares
    // the consumer's plain-rule base name with no alias to mask it.
    // The unreferenced weak helper must still be pruned.
    let path = fixture_path(
        "cddl/vectors/project/positive/plain_vs_generic_collision_unreferenced_unaliased_generic.cddl",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        !compiled.warnings.iter().any(|d| d.code == "E013"),
        "unaliased unreferenced weak imported generic helper must not collide with a strong local plain rule; \
         got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn generic_within_validates_expanded_group_instantiation() {
    let path = write_temp_file(
        "generic_within_group_valid.cddl",
        "root = wrapper<good>\n\
         wrapper<h> = protected<h> .within target\n\
         protected<h> = (protected: h, unprotected: {})\n\
         target = (protected: uint, unprotected: {})\n\
         good = 1\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        !compiled
            .warnings
            .iter()
            .any(|diagnostic| diagnostic.code == "E030"),
        "expanded generic .within should pass: {:#?}",
        compiled.warnings
    );
}

#[test]
fn within_substitution_inside_array_with_nested_generic() {
    // Reproduces the bug where a generic parameter appearing inside an
    // array entry is not substituted when the generic's body also has a
    // .within ctlop. This is a minimal version of the dntls-cose-sign
    // regression.
    //
    // The structure mirrors the dntls-cose-sign.cddl layout: a generic
    // signature whose body uses `bstr .dtrm` (so the substituted value
    // must be bstr-compatible), with a target that has the same shape.
    let path = write_temp_file(
        "generic_array_within.cddl",
        "root = sig<bstr_value, nil>\n\
         sig<headers, signature> =  [\n\
             headers,\n\
             signature: bstr .dtrm signature\n\
         ] .within cose_sig\n\
         cose_sig = [\n\
             protected: bstr,\n\
             unprotected: bstr,\n\
         ]\n\
         bstr_value = bstr\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        !compiled.warnings.iter().any(|w| w.code == "E030"),
        "expected no E030 (unresolved .within subtype), got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn within_check_sees_substituted_parameter_in_outer_generic() {
    // The generic body's first array entry references a parameter
    // directly (no memberkey). After substitution, the resolved LHS type
    // must show the substituted parameter, not the bare parameter name.
    let path = write_temp_file(
        "generic_array_param.cddl",
        "root = simple<Null-Headers>\n\
         simple<h> = [h] .within target\n\
         target = [uint]\n\
         Null-Headers = {}\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    let resolution = crate::build_resolution(&compiled.complete_nodes);
    let rendered = crate::render_to_string(
        &compiled.complete_nodes,
        &resolution,
        &crate::ConcretePolicy::default(),
    );

    assert!(
        rendered.contains("root = [\n  {\n  } ; from Null-Headers\n] .within [\n  uint\n]"),
        "expected expanded root to contain substituted Null-Headers, got:\n{rendered}"
    );
    assert!(
        !rendered.contains("simple<") && !rendered.contains("[h]"),
        "expanded output must not leak generic formal `h`, got:\n{rendered}"
    );
}

#[test]
fn within_check_inside_nested_generic_substitutes_correctly() {
    // Reproduces the dntls-cose-sign regression at a smaller scale: a
    // generic definition's body uses a generic parameter as a bare
    // array entry, and that generic is itself instantiated inside
    // another generic's body. The `.within` check on the inner generic
    // must see the substituted parameter, not the bare formal name.
    let path = write_temp_file(
        "nested_generic.cddl",
        "outer = inner<mid_sig>\n\
         mid_sig = COSE_Sig<Null-Headers, nil>\n\
         inner<s> = [s] .within target\n\
         COSE_Sig<headers, signature> =  [headers, signature: bstr .dtrm signature] .within cose_sig\n\
         cose_sig = [protected: bstr, unprotected: {}]\n\
         target = [uint, sig: bstr]\n\
         Null-Headers = {}\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    let resolution = crate::build_resolution(&compiled.complete_nodes);
    let rendered = crate::render_to_string(
        &compiled.complete_nodes,
        &resolution,
        &crate::ConcretePolicy::default(),
    );

    assert!(
        rendered.contains("{\n    }, ; from Null-Headers"),
        "expected nested generic expansion to substitute Null-Headers, got:\n{rendered}"
    );
    assert!(
        !rendered.contains("COSE_Sig<")
            && !rendered.contains("headers,")
            && !rendered.contains("signature: bstr .dtrm signature"),
        "expanded output must not leak generic formals, got:\n{rendered}"
    );
}

#[test]
fn generic_within_failure_points_at_instantiation() {
    let source = "root = wrapper<bad>\n\
         wrapper<h> = protected<h> .within target\n\
         protected<h> = (protected: h, unprotected: {})\n\
         target = (protected: uint, unprotected: {})\n\
         bad = tstr\n";
    let path = write_temp_file("generic_within_group_invalid.cddl", source);
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    let diagnostic = compiled
        .warnings
        .iter()
        .find(|diagnostic| diagnostic.code == "E030")
        .expect("expected instantiated generic .within failure");
    let span = diagnostic
        .span
        .as_ref()
        .expect("generic .within diagnostic should have call-site span");
    let template_offset = source
        .find("wrapper<h>")
        .expect("test source should contain generic template");
    assert!(
        span.start < template_offset,
        "diagnostic should point at `root = wrapper<bad>`, not the generic template: {diagnostic:#?}"
    );
}

#[test]
fn root_reachability_treats_generic_instantiations_as_references() {
    let path = write_temp_file(
        "root_reachability_generic_instantiation.cddl",
        "root = non-empty<distinguishedName>\n\
         non-empty<M> = (M) .and ({ + any => any })\n\
         distinguishedName = {}\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        !compiled.warnings.iter().any(|diagnostic| {
            diagnostic.code == "E020" && diagnostic.message.contains("`non-empty`")
        }),
        "{:#?}",
        compiled.warnings
    );
}

#[test]
fn root_reachability_treats_socket_augmentation_targets_as_reachable() {
    let path = write_temp_file(
        "root_reachability_socket_target.cddl",
        "root = [ + $keyType ]\n\
         $keyType /= rsaKeyType\n\
         rsaKeyType = {\n\
           PublicKeyLength: rsaKeySize\n\
         }\n\
         rsaKeySize = uint\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        !compiled.warnings.iter().any(|diagnostic| {
            diagnostic.code == "E020" && diagnostic.message.contains("`rsaKeyType`")
        }),
        "{:#?}",
        compiled.warnings
    );
    assert!(
        !compiled.warnings.iter().any(|diagnostic| {
            diagnostic.code == "E020" && diagnostic.message.contains("`rsaKeySize`")
        }),
        "{:#?}",
        compiled.warnings
    );
}

#[test]
fn root_reachability_keeps_all_socket_augmentation_arms_reachable() {
    let path = write_temp_file(
        "root_reachability_socket_arms.cddl",
        "root = [ + $keyType ]\n\
         $keyType /= rsaKeyType\n\
         $keyType /= ecdsaKeyType\n\
         rsaKeyType = tstr\n\
         ecdsaKeyType = tstr\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        !compiled.warnings.iter().any(|diagnostic| {
            diagnostic.code == "E020" && diagnostic.message.contains("`ecdsaKeyType`")
        }),
        "{:#?}",
        compiled.warnings
    );
}

#[test]
fn root_reachability_treats_choice_augmentation_members_as_reachable() {
    let path = write_temp_file(
        "root_reachability_choice_augmentation.cddl",
        "root = extendedKeyUsageType\n\
         extendedKeyUsageType /= \"serverAuth\"\n\
         extendedKeyUsageType /= oid\n\
         oid = text .regexp \"([0-2])((\\\\.0)|(\\\\.[1-9][0-9]*))*\"\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        !compiled.warnings.iter().any(|diagnostic| {
            diagnostic.code == "E020" && diagnostic.message.contains("`oid`")
        }),
        "{:#?}",
        compiled.warnings
    );
}

#[test]
fn root_reachability_reports_only_disconnected_component_roots() {
    let path = write_temp_file(
        "root_reachability_transitive_unreferenced.cddl",
        "Tag1004 = #6.1004(text .abnf full-date)\n\
         Tag0 = #6.0(text .abnf date-time)\n\
         full-date = \"full-date\" .cat rfc3339\n\
         date-time = \"date-time\" .cat rfc3339\n\
         rfc3339 = tstr\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        compiled.warnings.iter().any(|diagnostic| {
            diagnostic.code == "E020" && diagnostic.message.contains("`Tag0`")
        })
    );
    assert!(
        !compiled.warnings.iter().any(|diagnostic| {
            diagnostic.code == "E020" && diagnostic.message.contains("`date-time`")
        }),
        "{:#?}",
        compiled.warnings
    );
}

#[test]
fn generic_expansion_handles_nested_instantiations() {
    let path = write_temp_file(
        "generic_nested.cddl",
        "identity<t> = t\nadd-one<t> = identity<t> .plus 1\nbase = 2\nthis = add-one<base>\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(compiled.resolved_types.is_resolved("this"));
    assert_eq!(
        cache_entry(&compiled.resolved_types, "this"),
        EntryState::Integer(3)
    );
}

#[test]
fn generic_expansion_uses_imported_definitions() {
    let dir = std::env::temp_dir().join("cbork_compiler_test");
    std::fs::create_dir_all(&dir).unwrap();
    let lib_path = dir.join("generic_lib.cddl");
    std::fs::write(&lib_path, "add-one<t> = t .plus 1\n").unwrap();

    let path = write_temp_file(
        "generic_imported.cddl",
        "base = 2\n;# include \"./generic_lib.cddl\"\nthis = add-one<base>\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(compiled.resolved_types.is_resolved("this"));
    assert_eq!(
        cache_entry(&compiled.resolved_types, "this"),
        EntryState::Integer(3)
    );
}

#[test]
fn plain_and_generic_rule_names_collide_with_targeted_diagnostic() {
    let path = write_temp_file(
        "generic_name_collision.cddl",
        "this = this<any>\nthis<t> = bytes .cbor t\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    let diagnostic = compiled
        .warnings
        .iter()
        .find(|diagnostic| diagnostic.message.contains("rule name collision: `this`"))
        .expect("expected targeted plain/generic name collision diagnostic");

    assert_eq!(diagnostic.level, crate::DiagnosticLevel::Error);
    assert_eq!(diagnostic.source_file.as_ref(), Some(&path));
    assert!(
        diagnostic.span.is_some(),
        "collision diagnostic should carry a span"
    );

    let previous = diagnostic
        .previous_origin
        .as_ref()
        .expect("collision diagnostic should carry previous definition origin");
    assert_eq!(previous.source_path, path);
    assert_eq!(previous.line, 1);
    assert_eq!(previous.column, 1);
}

#[test]
fn structural_user_definition_collisions_emit_warning_and_error() {
    // Reference `argon2id-any` from a non-prunable root so the three
    // colliding definitions are reachable.  Without the reference all
    // three would be pruned silently before the collision check.
    let path = write_temp_file(
        "structural_definition_collisions.cddl",
        ";! `argon2id-options` carries only the parameters that need to survive on wire.\n\
         \n\
         root = argon2id-any\n\
         \n\
         argon2id-any = argon2id<any>\n\
         \n\
         argon2id-any = argon2id<any>\n\
         \n\
         argon2id<t> = any .dtrm (tagged-argon2id<t> / untagged-argon2id<t> )\n\
         \n\
         argon2id-any = bstr\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    let redundant = compiled
        .warnings
        .iter()
        .find(|diagnostic| {
            diagnostic.level == crate::DiagnosticLevel::Warning
                && diagnostic
                    .message
                    .contains("redundant definition of `argon2id-any`")
        })
        .expect("expected redundant definition warning");
    let conflict = compiled
        .warnings
        .iter()
        .find(|diagnostic| {
            diagnostic.level == crate::DiagnosticLevel::Error
                && diagnostic
                    .message
                    .contains("conflicting definition of `argon2id-any`")
        })
        .expect("expected conflicting definition error");

    assert_eq!(redundant.source_file.as_ref(), Some(&path));
    assert_eq!(conflict.source_file.as_ref(), Some(&path));
    assert!(redundant.previous_origin.is_some());
    assert!(conflict.previous_origin.is_some());
}

#[test]
fn choice_augmentations_do_not_emit_definition_collisions() {
    let path = write_temp_file(
        "choice_augmentations.cddl",
        "root = $message\n\
         $message /= [1, text]\n\
         $message /= [2, uint]\n\
         delivery //= (lat: float)\n\
         delivery //= (long: float)\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        !compiled.warnings.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("conflicting definition of `$message`")
                || diagnostic
                    .message
                    .contains("redundant definition of `$message`")
                || diagnostic
                    .message
                    .contains("conflicting definition of `delivery`")
                || diagnostic
                    .message
                    .contains("redundant definition of `delivery`")
        }),
        "{:#?}",
        compiled.warnings
    );
}

#[test]
fn undefined_socket_references_are_empty_not_errors() {
    let path = write_temp_file(
        "undefined_sockets.cddl",
        "root = [* $not-defined, * $$also-not-defined]\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        !compiled.warnings.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("undefined reference `$not-defined`")
                || diagnostic
                    .message
                    .contains("undefined reference `$$also-not-defined`")
        }),
        "{:#?}",
        compiled.warnings
    );
}

#[test]
fn socket_plugs_require_matching_augmentation_operator() {
    let path = write_temp_file(
        "socket_wrong_operator.cddl",
        "$type-socket //= (bad: uint)\n\
         $$group-socket /= uint\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    let type_socket = compiled
        .warnings
        .iter()
        .find(|diagnostic| {
            diagnostic.level == crate::DiagnosticLevel::Error
                && diagnostic.code == "E017"
                && diagnostic.message.contains("socket `$type-socket`")
                && diagnostic.message.contains("must be extended with `/=`")
        })
        .expect("expected type-socket operator diagnostic");
    let group_socket = compiled
        .warnings
        .iter()
        .find(|diagnostic| {
            diagnostic.level == crate::DiagnosticLevel::Error
                && diagnostic.code == "E017"
                && diagnostic.message.contains("socket `$$group-socket`")
                && diagnostic.message.contains("must be extended with `//=`")
        })
        .expect("expected group-socket operator diagnostic");

    assert_eq!(type_socket.source_file.as_ref(), Some(&path));
    assert_eq!(group_socket.source_file.as_ref(), Some(&path));
}

#[test]
fn ctlop_abnf_parses_text_controller() {
    let abnf_source = "rule = 1*ALPHA\n";
    let path = write_temp_file(
        "ctlop_abnf_text.cddl",
        "schema = text .abnf \"rule = 1*ALPHA\\n\"\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    assert!(compiled.warnings.is_empty(), "{:#?}", compiled.warnings);
    assert!(compiled.resolved_types.is_resolved("schema"));

    match cache_entry(&compiled.resolved_types, "schema") {
        EntryState::Abnf(document) => {
            assert_eq!(document.as_ref().source(), abnf_source);
            assert_eq!(document.as_ref().rules().len(), 1);
        },
        other => panic!("expected ABNF entry, got {other}"),
    }
}

#[test]
fn ctlop_abnf_parses_bytes_controller() {
    let abnf_source = "rule = 1*ALPHA\n";
    let path = write_temp_file(
        "ctlop_abnf_bytes.cddl",
        "schema = bytes .abnfb h'72756c65203d20312a414c5048410a'\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    assert!(compiled.warnings.is_empty(), "{:#?}", compiled.warnings);
    assert!(compiled.resolved_types.is_resolved("schema"));

    match cache_entry(&compiled.resolved_types, "schema") {
        EntryState::Abnf(document) => {
            assert_eq!(document.as_ref().source(), abnf_source);
            assert_eq!(document.as_ref().rules().len(), 1);
        },
        other => panic!("expected ABNF entry, got {other}"),
    }
}

#[test]
fn ctlop_abnf_rejects_non_string_lhs() {
    let path = write_temp_file(
        "ctlop_abnf_bad_lhs.cddl",
        "schema = 1 .abnf \"rule = 1*ALPHA\\n\"\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    let rule = find_rule_node(&compiled.user_nodes, "schema = 1 .abnf")
        .expect("expected the failing rule to remain in the tree");

    assert!(
        rule.metadata().contains(&MetaData::CtlopTypeMismatch),
        "non-string LHS should be rejected"
    );
    assert!(
        !compiled.resolved_types.is_resolved("schema"),
        "invalid ABNF definition should not resolve"
    );
}

#[test]
fn ctlop_abnf_rejects_non_utf8_bytes_rhs() {
    let path = write_temp_file("ctlop_abnf_bad_rhs.cddl", "schema = text .abnf h'ff'\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    let rule = find_rule_node(&compiled.user_nodes, "schema = text .abnf")
        .expect("expected the failing rule to remain in the tree");

    assert!(
        rule.metadata().contains(&MetaData::CtlopTypeMismatch),
        "non-UTF-8 RHS should be rejected"
    );
    assert!(
        !compiled.resolved_types.is_resolved("schema"),
        "invalid ABNF definition should not resolve"
    );
}

#[test]
fn ctlop_enc_abnf_parses_text_controller() {
    let abnf_source = "rule = 1*ALPHA\n";
    let path = write_temp_file(
        "ctlop_enc_abnf_text.cddl",
        "schema = bytes .x-enc.abnf \"rule = 1*ALPHA\\n\"\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    assert!(compiled.warnings.is_empty(), "{:#?}", compiled.warnings);
    assert!(compiled.resolved_types.is_resolved("schema"));

    match cache_entry(&compiled.resolved_types, "schema") {
        EntryState::EncAbnf(document) => {
            assert_eq!(document.as_ref().source(), abnf_source);
            assert_eq!(document.as_ref().rules().len(), 1);
        },
        other => panic!("expected EncABNF entry, got {other}"),
    }
}

#[test]
fn ctlop_hash_abnf_parses_bytes_controller() {
    let abnf_source = "rule = 1*ALPHA\n";
    let path = write_temp_file(
        "ctlop_hash_abnf_bytes.cddl",
        "schema = bytes .x-hash.abnfb h'72756c65203d20312a414c5048410a'\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    assert!(compiled.warnings.is_empty(), "{:#?}", compiled.warnings);
    assert!(compiled.resolved_types.is_resolved("schema"));

    match cache_entry(&compiled.resolved_types, "schema") {
        EntryState::HashAbnf(document) => {
            assert_eq!(document.as_ref().source(), abnf_source);
            assert_eq!(document.as_ref().rules().len(), 1);
        },
        other => panic!("expected HashABNF entry, got {other}"),
    }
}

#[test]
fn ctlop_enc_abnf_rejects_non_bytes_lhs() {
    let path = write_temp_file(
        "ctlop_enc_abnf_bad_lhs.cddl",
        "schema = text .x-enc.abnf \"rule = 1*ALPHA\\n\"\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    let rule = find_rule_node(&compiled.user_nodes, "schema = text .x-enc.abnf")
        .expect("expected the failing rule to remain in the tree");

    assert!(
        rule.metadata().contains(&MetaData::CtlopTypeMismatch),
        "non-bytes LHS should be rejected"
    );
    assert!(
        !compiled.resolved_types.is_resolved("schema"),
        "invalid annotated ABNF definition should not resolve"
    );
}

#[test]
fn ctlop_hash_abnf_rejects_non_utf8_bytes_rhs() {
    let path = write_temp_file(
        "ctlop_hash_abnf_bad_rhs.cddl",
        "schema = bytes .x-hash.abnf h'ff'\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    let rule = find_rule_node(&compiled.user_nodes, "schema = bytes .x-hash.abnf")
        .expect("expected the failing rule to remain in the tree");

    assert!(
        rule.metadata().contains(&MetaData::CtlopTypeMismatch),
        "non-UTF-8 RHS should be rejected"
    );
    assert!(
        !compiled.resolved_types.is_resolved("schema"),
        "invalid annotated ABNF definition should not resolve"
    );
}

#[test]
fn ctlop_regexp_parses_text_controller() {
    let path = write_temp_file(
        "ctlop_regexp_text.cddl",
        "schema = text .regexp \"[a-z]+\"\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    assert!(compiled.warnings.is_empty(), "{:#?}", compiled.warnings);
    assert!(compiled.resolved_types.is_resolved("schema"));

    match cache_entry(&compiled.resolved_types, "schema") {
        EntryState::Regex(regex) => {
            assert_eq!(regex.as_ref().source(), "[a-z]+");
            assert!(regex.as_ref().validate_text("abc").is_ok());
        },
        other => panic!("expected regex entry, got {other}"),
    }
}

#[test]
fn ctlop_regexp_parses_bytes_controller() {
    let path = write_temp_file(
        "ctlop_regexp_bytes.cddl",
        "schema = bytes .regexp h'5b612d7a5d2b'\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    assert!(compiled.warnings.is_empty(), "{:#?}", compiled.warnings);
    assert!(compiled.resolved_types.is_resolved("schema"));

    match cache_entry(&compiled.resolved_types, "schema") {
        EntryState::Regex(regex) => {
            assert_eq!(regex.as_ref().source(), "[a-z]+");
            assert!(regex.as_ref().validate_text("abc").is_ok());
        },
        other => panic!("expected regex entry, got {other}"),
    }
}

#[test]
fn ctlop_regexp_rejects_non_string_lhs() {
    let path = write_temp_file(
        "ctlop_regexp_bad_lhs.cddl",
        "schema = 1 .regexp \"[a-z]+\"\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    let rule = find_rule_node(&compiled.user_nodes, "schema = 1 .regexp")
        .expect("expected the failing rule to remain in the tree");

    assert!(
        rule.metadata().contains(&MetaData::CtlopTypeMismatch),
        "non-string LHS should be rejected"
    );
    assert!(
        !compiled.resolved_types.is_resolved("schema"),
        "invalid regexp definition should not resolve"
    );
}

#[test]
fn ctlop_regexp_rejects_non_utf8_bytes_rhs() {
    let path = write_temp_file("ctlop_regexp_bad_rhs.cddl", "schema = text .regexp h'ff'\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    let rule = find_rule_node(&compiled.user_nodes, "schema = text .regexp")
        .expect("expected the failing rule to remain in the tree");

    assert!(
        rule.metadata().contains(&MetaData::CtlopTypeMismatch),
        "non-UTF-8 RHS should be rejected"
    );
    assert!(
        !compiled.resolved_types.is_resolved("schema"),
        "invalid regexp definition should not resolve"
    );
}

#[test]
fn ctlop_enc_accepts_bytes_lhs() {
    let path = write_temp_file("ctlop_enc_bytes.cddl", "schema = bytes .x-enc any\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    assert!(compiled.warnings.is_empty(), "{:#?}", compiled.warnings);

    let rule = find_rule_node(&compiled.complete_nodes, "schema = bytes .x-enc")
        .expect("expected the encryption wrapper to remain in the tree");
    assert!(
        !rule.metadata().contains(&MetaData::CtlopTypeMismatch),
        "bytes LHS should be accepted for .x-enc"
    );
}

#[test]
fn ctlop_hash_accepts_bytes_lhs() {
    let path = write_temp_file("ctlop_hash_bytes.cddl", "schema = bytes .x-hash any\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    assert!(compiled.warnings.is_empty(), "{:#?}", compiled.warnings);

    let rule = find_rule_node(&compiled.complete_nodes, "schema = bytes .x-hash")
        .expect("expected the hash wrapper to remain in the tree");
    assert!(
        !rule.metadata().contains(&MetaData::CtlopTypeMismatch),
        "bytes LHS should be accepted for .x-hash"
    );
}

#[test]
fn ctlop_enc_rejects_non_annotation_lhs() {
    let path = write_temp_file("ctlop_enc_bad_lhs.cddl", "schema = any .x-enc any\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    let rule = find_rule_node(&compiled.complete_nodes, "schema = any .x-enc")
        .expect("expected the failing rule to remain in the tree");

    assert!(
        rule.metadata().contains(&MetaData::CtlopTypeMismatch),
        "any LHS should be rejected"
    );
}

#[test]
fn finalization_injects_referenced_postlude_only() {
    let path = write_temp_file("postlude_injection.cddl", "schema = eb64legacy\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        find_rule_node(&compiled.user_nodes, "eb64legacy =").is_none(),
        "user tree should not bulk-inject unused postlude content"
    );

    let injected = find_rule_node(&compiled.complete_nodes, "eb64legacy =")
        .expect("expected the referenced postlude definition to be injected");
    assert!(
        injected.metadata().contains(&MetaData::StandardPostlude),
        "injected postlude definition should be tagged"
    );
}

#[test]
fn finalization_continues_after_earlier_mismatch() {
    let path = write_temp_file(
        "postlude_after_mismatch.cddl",
        "schema = text .abnf h'ff'\nroot = eb64legacy\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    let bad_rule = find_rule_node(&compiled.user_nodes, "schema = text .abnf")
        .expect("expected the invalid ctlop to remain in the tree");
    assert!(
        bad_rule.metadata().contains(&MetaData::CtlopTypeMismatch),
        "earlier ctlop mismatch should still be recorded"
    );

    let injected = find_rule_node(&compiled.complete_nodes, "eb64legacy =")
        .expect("expected the postlude definition to be injected even after earlier mismatches");
    assert!(
        injected.metadata().contains(&MetaData::StandardPostlude),
        "injected postlude should still be marked"
    );
}

/// Recursive postlude dependencies: referencing `int` from the user tree must
/// pull in `uint` and `nint` transitively because the injection loop runs
/// until a fixed point and the postlude's `int` itself references both names.
#[test]
fn postlude_injects_recursive_dependencies() {
    let path = write_temp_file("postlude_recursive_int.cddl", "root = int\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    let int_def = find_rule_node(&compiled.complete_nodes, "int =")
        .expect("expected postlude `int` to be injected");
    assert!(
        int_def.metadata().contains(&MetaData::StandardPostlude),
        "injected `int` should carry the StandardPostlude tag"
    );
    assert!(
        int_def.metadata().contains(&MetaData::Silent),
        "injected `int` should be silent for concise emission"
    );

    let uint_def = find_rule_node(&compiled.complete_nodes, "uint =")
        .expect("expected postlude `uint` to be transitively injected");
    assert!(
        uint_def.metadata().contains(&MetaData::StandardPostlude),
        "injected `uint` should carry the StandardPostlude tag"
    );

    let nint_def = find_rule_node(&compiled.complete_nodes, "nint =")
        .expect("expected postlude `nint` to be transitively injected");
    assert!(
        nint_def.metadata().contains(&MetaData::StandardPostlude),
        "injected `nint` should carry the StandardPostlude tag"
    );
}

/// Multi-level transitive postlude dependencies: `float` depends on
/// `float16-32` and `float64`; `float16-32` depends on `float16` and
/// `float32`.  All four transitively-referenced names must be in the
/// complete tree, all marked as standard postlude.
#[test]
fn postlude_injects_multi_level_transitive_dependencies() {
    let path = write_temp_file("postlude_recursive_float.cddl", "root = float\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    for name in ["float", "float16-32", "float64", "float16", "float32"] {
        let needle = format!("{name} =");
        let rule = find_rule_node(&compiled.complete_nodes, &needle)
            .unwrap_or_else(|| panic!("expected postlude `{name}` to be injected"));
        assert!(
            rule.metadata().contains(&MetaData::StandardPostlude),
            "injected `{name}` should carry the StandardPostlude tag"
        );
    }
}

/// User redefines `bytes` to be `bstr` (same structural content as the
/// postlude's `bytes = bstr`).  The postlude's `bytes` must not be injected
/// over the user definition, and the user definition must be flagged as
/// redundant so the user knows the redefinition is unnecessary.
#[test]
fn postlude_user_redefinition_with_matching_content_is_redundant() {
    let path = write_temp_file("postlude_match.cddl", "root = bytes\nbytes = bstr\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    let user_bytes = find_rule_node(&compiled.user_nodes, "bytes = bstr")
        .expect("user `bytes = bstr` should remain in the user tree");
    assert!(
        user_bytes
            .metadata()
            .contains(&MetaData::RedundantDefinition),
        "user `bytes = bstr` should be tagged redundant"
    );
    assert!(
        compiled
            .warnings
            .iter()
            .any(|d| d.code == "W001" && d.message.contains("`bytes`")),
        "expected a redundant-definition warning for `bytes`, got: {:#?}",
        compiled.warnings
    );

    // The postlude's `bytes` must not be injected because the user already
    // defines it.  The postlude's `bstr` *should* still be injected because
    // the user's `bstr` reference is the first time it is mentioned.
    let postlude_bytes = find_rule_node(&compiled.complete_nodes, "bytes = bstr").filter(|node| {
        if let WrappedNode::RuleLine { text, origin, .. } = node {
            origin.source_path.to_string_lossy() == "<postlude>" && text.trim() == "bytes = bstr"
        } else {
            false
        }
    });
    assert!(
        postlude_bytes.is_none(),
        "the postlude's `bytes` must not be injected over the user definition; \
         found: {postlude_bytes:?}"
    );

    let bstr = find_rule_node(&compiled.complete_nodes, "bstr =")
        .expect("postlude `bstr` should be injected because `bytes = bstr` references it");
    assert!(
        bstr.metadata().contains(&MetaData::StandardPostlude),
        "transitively-injected `bstr` should be tagged"
    );
}

/// User redefines a postlude name with a different signature and no
/// ctlop-form / tag-form equivalence.  The user definition is kept
/// verbatim, the postlude is not injected over it, and a hard
/// `ConflictingDefinition` error is reported so the user is told their
/// override is incompatible with the postlude.
#[test]
fn postlude_user_override_with_different_signature_is_conflict() {
    let path = write_temp_file("postlude_override.cddl", "root = bytes\nbytes = 42\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    let user_bytes = find_rule_node(&compiled.user_nodes, "bytes = 42")
        .expect("user `bytes = 42` should remain in the user tree");
    assert!(
        user_bytes
            .metadata()
            .contains(&MetaData::ConflictingDefinition),
        "user `bytes = 42` should be tagged conflicting: \
         the override is not equivalent to the postlude's `bytes = bstr`"
    );
    assert!(
        compiled
            .warnings
            .iter()
            .any(|d| d.code == "E014" && d.message.contains("`bytes`")),
        "expected a conflicting-definition error for `bytes`, got: {:#?}",
        compiled.warnings
    );
    assert!(
        !user_bytes
            .metadata()
            .contains(&MetaData::RedundantDefinition),
        "user `bytes = 42` has a different signature from the postlude, so it must not be tagged redundant"
    );

    // The postlude's `bytes` must not be injected over the user definition.
    let postlude_bytes = find_rule_node(&compiled.complete_nodes, "bytes = bstr")
        .filter(|node| node.origin().source_path.to_string_lossy() == "<postlude>");
    assert!(
        postlude_bytes.is_none(),
        "the postlude's `bytes` must not be injected over the user definition"
    );
}

/// RFC 8610 §3.8.4 documents `encoded-cbor = bytes .cbor type1` (where
/// `type1 = bstr`) as the ctlop-form re-statement of the postlude's
/// tag-form `encoded-cbor = #6.24(bstr)`.  The two are *not* equivalent
/// in this file because the ctlop's argument is `type1` (a locally
/// bound `bstr`), not the postlude's `any` — so the user has narrowed
/// the payload type.  The compiler must surface this as a hard
/// `ConflictingDefinition` so the user is told their ctlop-form
/// `encoded-cbor` is *not* the same as the postlude's tag-form.  The
/// postlude must not be injected over the user definition.
#[test]
fn postlude_rfc8610_encoded_cbor_ctlop_form_with_non_any_argument_is_conflict() {
    let path = write_temp_file(
        "rfc8610_encoded_cbor.cddl",
        "rfc8610_cbor = encoded-cbor / encoded-cborseq\n\n\
         ; source: RFC 8610 Section 3.8.4\n\
         encoded-cbor = bytes .cbor type1\n\
         encoded-cborseq = bytes .cborseq type2\n\n\
         type1 = bstr\n\
         type2 = tstr\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    let user_encoded_cbor = find_rule_node(&compiled.user_nodes, "encoded-cbor = bytes .cbor")
        .expect("user `encoded-cbor` should remain in the user tree");
    assert!(
        user_encoded_cbor
            .metadata()
            .contains(&MetaData::ConflictingDefinition),
        "the user's `encoded-cbor = bytes .cbor type1` is not the ctlop-form \
         equivalent of the postlude's tag-form (its argument is `type1`, not `any`), \
         so it must be tagged as conflicting"
    );
    assert!(
        compiled
            .warnings
            .iter()
            .any(|d| d.code == "E014" && d.message.contains("`encoded-cbor`")),
        "expected an E014 conflict for `encoded-cbor`, got: {:#?}",
        compiled.warnings
    );
    assert!(
        !user_encoded_cbor
            .metadata()
            .contains(&MetaData::RedundantDefinition),
        "the ctlop-form with a non-`any` argument is a different definition, \
         so it must not be tagged as redundant"
    );

    // The postlude's `encoded-cbor` must not be injected over the user
    // definition, because the user has already defined the name.
    let postlude_encoded_cbor = find_rule_node(&compiled.complete_nodes, "encoded-cbor = #6.24")
        .filter(|node| node.origin().source_path.to_string_lossy() == "<postlude>");
    assert!(
        postlude_encoded_cbor.is_none(),
        "the postlude's tag-form `encoded-cbor` must not be injected over \
         the user's ctlop-form definition; found: {postlude_encoded_cbor:?}"
    );
}

/// The ctlop-form `encoded-cbor = bytes .cbor any` *is* the RFC 8610
/// §3.8.4 ctlop-form equivalent of the postlude's tag-form
/// `encoded-cbor = #6.24(bstr)`: both say "a bstr containing a CBOR
/// data item", and the ctlop's argument is the postlude's catch-all
/// `any` type.  The user's redefinition is therefore a redundant
/// restatement of the postlude — the user could simply delete the
/// `encoded-cbor = bytes .cbor any` line and rely on the postlude.
#[test]
fn postlude_user_ctlop_form_with_any_argument_is_redundant() {
    let path = write_temp_file(
        "postlude_ctlop_any.cddl",
        "root = encoded-cbor\nencoded-cbor = bytes .cbor any\n",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    let user_encoded_cbor = find_rule_node(&compiled.user_nodes, "encoded-cbor = bytes .cbor any")
        .expect("user `encoded-cbor = bytes .cbor any` should remain in the user tree");
    assert!(
        user_encoded_cbor
            .metadata()
            .contains(&MetaData::RedundantDefinition),
        "user `encoded-cbor = bytes .cbor any` is the ctlop-form restatement of \
         the postlude's tag-form `encoded-cbor = #6.24(bstr)`, so it must be \
         tagged as redundant"
    );
    assert!(
        !user_encoded_cbor
            .metadata()
            .contains(&MetaData::ConflictingDefinition),
        "the ctlop-form restatement must not be tagged as conflicting"
    );
    assert!(
        compiled
            .warnings
            .iter()
            .any(|d| d.code == "W001" && d.message.contains("`encoded-cbor`")),
        "expected a W001 redundant warning for `encoded-cbor`, got: {:#?}",
        compiled.warnings
    );
    assert!(
        !compiled
            .warnings
            .iter()
            .any(|d| d.code == "E014" && d.message.contains("`encoded-cbor`")),
        "no E014 conflict should fire for the ctlop-form restatement"
    );
}

/// A retained reference that is undefined and not present in the standard
/// postlude must surface as a hard dangling-reference error (E016).
#[test]
fn postlude_dangling_reference_outside_postlude_is_error() {
    let path = write_temp_file("postlude_dangling.cddl", "root = missing_type\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        compiled.warnings.iter().any(|d| {
            d.code == "E016" && d.message.contains("undefined reference `missing_type`")
        }),
        "expected E016 for `missing_type`, got: {:#?}",
        compiled.warnings
    );
}

/// After the injection loop reaches its fixed point, any still-undefined
/// reference must have been reported; a referenced postlude name that is
/// reached must be present in the complete tree, and an unreachable one
/// (referenced only from a pruned rule) must not appear.
#[test]
fn postlude_injection_reaches_fixed_point() {
    // `root = int` references `int`, which references `uint` and `nint`.
    // After the injection loop terminates, all three postlude rules must
    // be present in the complete tree.
    let path = write_temp_file("postlude_fixed_point.cddl", "root = int\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    for name in ["int", "uint", "nint"] {
        assert!(
            find_rule_node(&compiled.complete_nodes, &format!("{name} =")).is_some(),
            "postlude `{name}` should be in the complete tree at the fixed point"
        );
    }

    // An unreferenced postlude name must NOT be injected (no bulk-injection).
    assert!(
        find_rule_node(&compiled.complete_nodes, "bigint =").is_none(),
        "unreferenced postlude `bigint` must not be bulk-injected"
    );
    assert!(
        find_rule_node(&compiled.complete_nodes, "decfrac =").is_none(),
        "unreferenced postlude `decfrac` must not be bulk-injected"
    );
}

/// Injected postlude nodes must be silent for concise emission: every node
/// in the injected subtree, not only the top-level rule, must carry
/// [`MetaData::Silent`].  Downstream renderers rely on this to keep the
/// emitted CDDL free of standard-prelude noise.
#[test]
fn postlude_injected_subtree_is_fully_silent() {
    fn assert_silent(
        node: &WrappedNode,
        context: &str,
    ) {
        match node {
            WrappedNode::RuleLine { metadata, .. }
            | WrappedNode::Comment { metadata, .. }
            | WrappedNode::Syntax { metadata, .. }
            | WrappedNode::Directive { metadata, .. }
            | WrappedNode::ModuleStart { metadata, .. }
            | WrappedNode::ModuleEnd { metadata, .. } => {
                assert!(
                    metadata.contains(&MetaData::Silent),
                    "{context} should be Silent, got metadata {metadata:?}"
                );
            },
        }
        if let WrappedNode::RuleLine { children, .. }
        | WrappedNode::Syntax { children, .. }
        | WrappedNode::Directive { children, .. } = node
        {
            for (i, child) in children.iter().enumerate() {
                assert_silent(child, &format!("{context} child {i}"));
            }
        }
    }

    let path = write_temp_file("postlude_silent.cddl", "root = int\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    for node in &compiled.complete_nodes {
        if node.origin().source_path.to_string_lossy() == "<postlude>" {
            assert_silent(node, "postlude node");
        }
    }
}

/// Alias/prefix interaction: when the user file imports a library via an
/// alias (e.g. `rfcXXXX as cose`) and the user references a postlude-backed
/// type directly, the postlude merge must still inject the right definitions
/// regardless of the alias.  The alias only renames the imported library
/// namespace; postlude types are not namespaced.
#[test]
fn postlude_works_independent_of_import_aliases() {
    let dir = std::env::temp_dir().join("cbork_compiler_test");
    std::fs::create_dir_all(&dir).unwrap();
    let lib_path = dir.join("postlude_alias_lib.cddl");
    std::fs::write(&lib_path, "cose_wrapper = tstr\n").unwrap();
    let path = write_temp_file(
        "postlude_alias_user.cddl",
        "root = text\n;# import \"./postlude_alias_lib.cddl\" as cose\n",
    );

    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    // The postlude's `text` and `tstr` must be injected (text -> tstr transitively).
    let text_def = find_rule_node(&compiled.complete_nodes, "text =")
        .expect("postlude `text` should be injected");
    assert!(text_def.metadata().contains(&MetaData::StandardPostlude));
    let tstr_def = find_rule_node(&compiled.complete_nodes, "tstr =")
        .expect("postlude `tstr` should be transitively injected");
    assert!(tstr_def.metadata().contains(&MetaData::StandardPostlude));
}

/// When the user file defines a postlude name with the exact same signature
/// as the postlude, the user wins, the postlude is not injected, and the
/// `RedundantDefinition` tag is set on the user definition.  Crucially, the
/// redundant-detection must still fire for tag-based postlude types whose
/// RHS is a tag reference, not just for postlude types that happen to
/// resolve to a concrete value in the cache.
#[test]
fn postlude_redundancy_detection_works_for_tag_based_types() {
    // `bstr = #2` in the postlude.  The user redefines it with the same
    // tag literal.  Both have the same structural signature, so the user
    // definition must be flagged redundant.
    let path = write_temp_file("postlude_redundant_tag.cddl", "root = bstr\nbstr = #2\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    let user_bstr = find_rule_node(&compiled.user_nodes, "bstr = #2")
        .expect("user `bstr = #2` should remain in the user tree");
    assert!(
        user_bstr
            .metadata()
            .contains(&MetaData::RedundantDefinition),
        "user `bstr = #2` should be tagged redundant against the postlude `bstr`"
    );
    assert!(
        compiled
            .warnings
            .iter()
            .any(|d| d.code == "W001" && d.message.contains("`bstr`")),
        "expected a redundant-definition warning for `bstr`, got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn same_origin_import_convergence_lints_cleanly() {
    // Step 5.9 regression: importing the same helper file through two
    // paths that resolve to the same canonical file must not emit W001
    // for the converged rule.
    let path =
        fixture_path("cddl/vectors/project/positive/same_origin_convergence_effective_name.cddl");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        !compiled.warnings.iter().any(|d| d.code == "W001"),
        "same-origin import convergence must not emit W001; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn same_origin_well_known_convergence_lints_cleanly() {
    // Step 5.9 regression: importing the same well-known module through
    // two named selectors that share a leaf rule must not emit W001.
    let path =
        fixture_path("cddl/vectors/project/positive/same_origin_well_known_convergence.cddl");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        !compiled.warnings.iter().any(|d| d.code == "W001"),
        "same-origin well-known convergence must not emit W001; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn distinct_origin_duplicate_emits_w001() {
    // Step 5.9 negative regression: two different source files defining
    // the same rule with matching content still emit W001.
    let path = fixture_path("cddl/vectors/project/semantic-errors/distinct_origin_duplicate.cddl");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        compiled.warnings.iter().any(|d| d.code == "W001"),
        "distinct-origin duplicate must emit W001; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn distinct_origin_conflict_emits_e014() {
    // Step 5.9 negative regression: two different source files defining
    // the same rule with differing content emit E014.
    let path = fixture_path("cddl/vectors/project/semantic-errors/distinct_origin_conflict.cddl");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        compiled.warnings.iter().any(|d| d.code == "E014"),
        "distinct-origin conflict must emit E014; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn bare_group_reference_in_map_lints_cleanly() {
    // Step 5.10 positive regression: a bare group reference inside a
    // map body must be expanded as normal CDDL group inclusion.  The
    // lint must not emit E030 or any reference diagnostic.
    let path = fixture_path("cddl/vectors/project/positive/bare_group_reference_in_map.cddl");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        !compiled.warnings.iter().any(|d| d.code == "E030"),
        "bare group reference in map must not trigger a cycle diagnostic; got: {:#?}",
        compiled.warnings
    );
    assert!(
        !compiled.warnings.iter().any(|d| d.code == "E016"),
        "bare group reference in map must resolve cleanly; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn bare_group_reference_with_within_lints_cleanly() {
    // Step 5.10 positive regression: a `.within` check where the LHS
    // map includes a bare group reference.  The effective LHS view
    // must reflect the expanded entries for the check to pass.
    let path = fixture_path("cddl/vectors/project/positive/bare_group_reference_with_within.cddl");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        !compiled.warnings.iter().any(|d| d.code == "E030"),
        "bare group reference in `.within` must not trigger a cycle diagnostic; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn bare_group_reference_cycle_emits_e030() {
    // Step 5.10 negative regression: a recursive bare group
    // reference cycle must produce an E030 diagnostic without
    // stack-overflowing the renderer.
    let path = fixture_path("cddl/vectors/project/semantic-errors/bare_group_reference_cycle.cddl");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        compiled.warnings.iter().any(|d| d.code == "E030"),
        "recursive bare group cycle must emit E030; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn transform_x_enc_within_bstr_lints_cleanly() {
    // Step 5.11 positive regression: `.x-enc` wrapper on the LHS
    // narrows the carrier; the RHS is the bare carrier `bstr` and
    // must accept the narrowed LHS.
    let path = fixture_path("cddl/vectors/project/positive/transform_x_enc_within_bstr.cddl");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        !compiled.warnings.iter().any(|d| d.code == "E030"),
        ".x-enc within bare bstr must lint cleanly; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn transform_x_enc_within_x_enc_lints_cleanly() {
    // Step 5.11 positive regression: same-family transforms must
    // subtype when their controllers subtype.
    let path = fixture_path("cddl/vectors/project/positive/transform_x_enc_within_x_enc.cddl");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        !compiled.warnings.iter().any(|d| d.code == "E030"),
        ".x-enc within .x-enc (compatible controllers) must lint cleanly; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn transform_x_hash_within_x_hash_lints_cleanly() {
    // Step 5.11 positive regression: `.x-hash` must subtype
    // `.x-hash` when the controllers subtype.
    let path = fixture_path("cddl/vectors/project/positive/transform_x_hash_within_x_hash.cddl");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        !compiled.warnings.iter().any(|d| d.code == "E030"),
        ".x-hash within .x-hash (compatible controllers) must lint cleanly; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn transform_x_brotli_within_x_compressed_lints_cleanly() {
    // Step 5.11 positive regression: a named compression algorithm
    // is within the generic `.x-compressed` wrapper when the
    // controllers subtype.
    let path =
        fixture_path("cddl/vectors/project/positive/transform_x_brotli_within_x_compressed.cddl");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        !compiled.warnings.iter().any(|d| d.code == "E030"),
        ".x-brotli within .x-compressed must lint cleanly; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn transform_x_compressed_within_x_brotli_emits_e030() {
    // Step 5.11 negative regression: `.x-compressed` is broader
    // than any named compression algorithm.
    let path = fixture_path(
        "cddl/vectors/project/semantic-errors/transform_x_compressed_within_x_brotli.cddl",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        compiled.warnings.iter().any(|d| d.code == "E030"),
        ".x-compressed within .x-brotli must emit E030; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn transform_x_brotli_within_x_zstd_emits_e030() {
    // Step 5.11 negative regression: two different named compression
    // algorithms are not mutually within each other.
    let path =
        fixture_path("cddl/vectors/project/semantic-errors/transform_x_brotli_within_x_zstd.cddl");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        compiled.warnings.iter().any(|d| d.code == "E030"),
        ".x-brotli within .x-zstd must emit E030; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn transform_x_enc_within_x_hash_emits_e030() {
    // Step 5.11 negative regression: `.x-enc` and `.x-hash` belong
    // to distinct transform families.
    let path =
        fixture_path("cddl/vectors/project/semantic-errors/transform_x_enc_within_x_hash.cddl");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        compiled.warnings.iter().any(|d| d.code == "E030"),
        ".x-enc within .x-hash must emit E030; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn transform_x_enc_within_x_brotli_emits_e030() {
    // Step 5.11 negative regression: `.x-enc` (encryption) is not
    // within `.x-brotli` (compression).
    let path =
        fixture_path("cddl/vectors/project/semantic-errors/transform_x_enc_within_x_brotli.cddl");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        compiled.warnings.iter().any(|d| d.code == "E030"),
        ".x-enc within .x-brotli must emit E030; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn transform_x_hash_within_x_compressed_emits_e030() {
    // Step 5.11 negative regression: `.x-hash` is not within
    // `.x-compressed`.
    let path = fixture_path(
        "cddl/vectors/project/semantic-errors/transform_x_hash_within_x_compressed.cddl",
    );
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        compiled.warnings.iter().any(|d| d.code == "E030"),
        ".x-hash within .x-compressed must emit E030; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn cbork_export_marks_rule_lints_cleanly() {
    // Step 5.12 positive regression: `;@ CBORK: Export` applied
    // before a rule in a library file lints cleanly and tags the
    // rule as part of the library's export surface.
    let path = fixture_path("cddl/vectors/project/positive/cbork_export_marks_rule.cddl");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        compiled.is_library,
        "fixture must declare itself as a library; got: {:#?}",
        compiled.warnings
    );
    assert!(
        compiled.exported_names.contains("public-rule"),
        "public-rule must be exported; got exported_names={:?}",
        compiled.exported_names
    );
    assert!(
        !compiled.exported_names.contains("private-helper"),
        "private-helper must NOT be exported; got exported_names={:?}",
        compiled.exported_names
    );
    // BUG-001 regression: a valid `;@ CBORK: Export` immediately
    // before a rule must not emit a false E020 "`;@ CBORK: Export`
    // must be applied to the next rule, not to another directive".
    // That diagnostic used to fire unconditionally from
    // `scan_cbork_file_directives`; the real diagnostic path is
    // E022 in `apply_export_directives_inner` (export at EOF,
    // export before an `import` / `include` directive, export in
    // a non-library file).  We check for the *specific* false
    // diagnostic by message, not just the code, because the
    // legitimate "unreferenced top-level definition" E020 lives
    // elsewhere in the pipeline.
    for diagnostic in &compiled.warnings {
        if diagnostic.code == "E020"
            && diagnostic
                .message
                .contains("must be applied to the next rule")
        {
            panic!("BUG-001 regression: false E020 emitted for valid export; got: {diagnostic:#?}");
        }
    }
}

#[test]
fn bug_001_cbork_export_before_rule_lints_cleanly() {
    // BUG-001 regression: a valid `;@ CBORK: Export` immediately
    // before a rule must not emit a false E020 in strict CLI
    // lint.  The fixture in `cddl/vectors/project/bugs/` mirrors
    // the canonical DNTLS / COSE fixture pattern (Library + Export
    // + rule) and must compile to a `CompiledCDDL` with the rule
    // recorded as exported and no `E020` "applied to the next
    // rule" diagnostic.
    let path = fixture_path("cddl/vectors/project/bugs/cbork_export_before_rule_false_e020.cddl");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        compiled.is_library,
        "fixture must declare itself as a library; got: {:#?}",
        compiled.warnings
    );
    assert!(
        compiled.exported_names.contains("public-rule"),
        "public-rule must be exported; got exported_names={:?}",
        compiled.exported_names
    );
    // The strict-mode bug was an E020 falsely emitted from
    // `scan_cbork_file_directives` for every Export site.  With
    // the fix in place, no diagnostic with that exact wording
    // may appear.
    for diagnostic in &compiled.warnings {
        assert!(
            !(diagnostic.code == "E020"
                && diagnostic
                    .message
                    .contains("must be applied to the next rule")),
            "BUG-001 regression: false E020 emitted for valid export; got: {diagnostic:#?}"
        );
    }
    // Strict-lint invariant: no hard errors at all.  The fixture
    // also references `private-helper` from `public-rule` so no
    // unreferenced-definition E020 should fire either.
    assert!(
        !compiled.has_errors(),
        "BUG-001 strict-mode invariant: no hard errors expected; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn bug_002_whole_library_import_does_not_emit_w006() {
    // BUG-002 regression: a consumer that imports a whole library
    // (no `from` clause) and references a non-exported private
    // helper from that library must not trigger a W006
    // "unused library export" warning.  A library export is a
    // public API surface, not an obligation for every consumer of
    // the library to reference every exported name.  Before the
    // fix, the unused `public-a` export triggered a false W006.
    let path = fixture_path("cddl/vectors/project/bugs/bug_002_whole_library_import_no_w006.cddl");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        compiled
            .imported_libraries
            .iter()
            .any(|lib| { lib.is_library && lib.exported_names.contains("public-a") }),
        "library must be registered with the `public-a` export; got: {:#?}",
        compiled.imported_libraries
    );
    // Whole-library import: the directive IS used (it brings in
    // `private-helper`), so W004 must NOT fire.
    assert!(
        !compiled.warnings.iter().any(|d| d.code == "W004"),
        "BUG-002 regression: whole-library import that is used must not emit W004; got: {:#?}",
        compiled.warnings
    );
    // No `from` clause: W005 (per-selected-name unused) must NOT fire.
    assert!(
        !compiled.warnings.iter().any(|d| d.code == "W005"),
        "BUG-002 regression: no `from` clause must not emit W005; got: {:#?}",
        compiled.warnings
    );
    // The library's `public-a` export is unreferenced by this
    // consumer, but that is the BUG: a library export is a
    // public API surface, not an obligation for this consumer.
    assert!(
        !compiled.warnings.iter().any(|d| d.code == "W006"),
        "BUG-002 regression: W006 must not fire for whole-library imports; got: {:#?}",
        compiled.warnings
    );
    // W003 (cross-file direct-use export) may still fire because
    // the consumer references the non-exported `private-helper`.
    // That diagnostic is the legitimate contract for whole-library
    // imports and is not part of the BUG-002 fix.
    assert!(
        compiled.warnings.iter().any(|d| d.code == "W003"),
        "BUG-002 fixture must trigger W003 for the non-exported reference; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn bug_003_alias_generic_helper_closure_lints_cleanly() {
    // BUG-003 regression: a consumer that imports a module whose
    // typename references point through a generic helper (e.g.
    // `via-alias = a.Wrapper<inner-type>`) must reach the inner
    // rule without emitting E016 (unresolved typename).  The wrap
    // function must:
    //   * strip generic-argument lists before comparing typename text against local rule
    //     names (`tagged<t>` -> `tagged`);
    //   * not re-prefix an already-qualified reference (`a.Wrapper` must stay `a.Wrapper`
    //     when the consumer's alias is `middle`, not become `middle.a.Wrapper`);
    //   * not recurse into Directive node children, which already carry the importer's final
    //     shape.
    let path = fixture_path("cddl/vectors/project/bugs/bug_003_alias_generic_helper_closure.cddl");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        !compiled.has_errors(),
        "BUG-003 regression: closure consumer must compile without errors; got: {:#?}",
        compiled.warnings
    );
    assert!(
        !compiled.warnings.iter().any(|d| d.code == "E016"),
        "BUG-003 regression: no E016 (unresolved typename); got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn cbork_export_at_eof_emits_e022() {
    // Step 5.12 negative regression: `;@ CBORK: Export` at EOF
    // (no following rule) must produce an E022 diagnostic.
    let path = fixture_path("cddl/vectors/project/semantic-errors/cbork_export_at_eof.cddl");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        compiled.warnings.iter().any(|d| d.code == "E022"),
        "Export at EOF must emit E022; got: {:#?}",
        compiled.warnings
    );
}

use crate::{Subdiag, SubdiagKind};

#[test]
fn bug_007_within_marks_failing_statement_with_conflict_line() {
    // BUG-007 regression: a `.within` failure whose conflict is
    // on a nested map/group entry inside the LHS must mark the
    // failing rendered line as the conflict-attributed subdiag.
    // Before the fix, every line was `Matched` / `Context` and the
    // only attribution was a pathless REASON note carrying `map[i]: ...`
    // text.  After the fix the diagnostic must include an
    // `Unmatched`-kind subdiag whose snippet is the failing rendered
    // LHS line and whose reason names the failed field.
    let path =
        fixture_path("cddl/vectors/project/bugs/bug_007_within_marks_failing_statement.cddl");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    let e030 = compiled
        .warnings
        .iter()
        .find(|d| d.code == "E030")
        .expect("BUG-007 fixture must emit an E030 diagnostic");

    // Find an `Unmatched` subdiag that names the failing entry.
    let mut found_unmatched_with_reason: Option<&Subdiag> = None;
    let mut found_any_unmatched = false;
    for sub in &e030.related {
        if matches!(sub.kind, SubdiagKind::Unmatched) {
            found_any_unmatched = true;
            if sub.snippet.contains("payload: bstr")
                || sub.snippet.contains("map[0]: LHS required entry")
                || sub.snippet.contains("map[1]")
            {
                found_unmatched_with_reason = Some(sub);
            }
        }
    }
    assert!(
        found_any_unmatched,
        "BUG-007 regression: DIFF must mark a failing line as Unmatched (the CLI renders this as CONFLICT); got: {e030:#?}"
    );
    assert!(
        found_unmatched_with_reason.is_some(),
        "BUG-007 regression: an Unmatched subdiag must name the failing field (`payload: bstr` or `map[i]`); got: {e030:#?}"
    );
}

#[test]
fn bug_009_bareword_memberkey_does_not_emit_unresolved_name() {
    // BUG-009 regression: a bareword map member key (`tld:`) is a
    // concrete text label, not a type reference.  A `.within`
    // comparison between a map with bareword keys and a generic
    // header pattern that admits text keys must succeed without
    // surfacing an `E030` for `unresolved name: tld`.
    let path = fixture_path("cddl/vectors/project/bugs/bug_009_bareword_memberkey_unresolved.cddl");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        !compiled.warnings.iter().any(|d| d.code == "E030"),
        "BUG-009 regression: bareword member keys must not produce `.within` failures with `unresolved name`; got: {:#?}",
        compiled.warnings
    );
    // The fixture also leaves `specific` unreferenced so the lint
    // is allowed to surface an E020.  Whatever happens, no `.within`
    // diagnostic should be emitted.
}

#[test]
fn bug_011_non_library_import_emits_w007_and_w003() {
    // BUG-011 regression: a directly-imported or directly-included
    // file that is not marked `;@ CBORK: Library` must surface a
    // W007 warning at the directive origin AND a W003 warning
    // (cross-file direct-use of a non-exported symbol) when the
    // consumer references a symbol that lives outside any
    // declared export surface.
    let import_path =
        fixture_path("cddl/vectors/project/semantic-errors/import_non_library_warns.cddl");
    let import_compiled = CompiledCDDL::compile(&import_path, None::<&Path>).unwrap();
    assert!(
        import_compiled
            .warnings
            .iter()
            .any(|d| d.code == "W007" && d.message.contains("non_library_time.cddl")),
        "BUG-011 regression: direct `import` of a non-library file must emit W007; got: {:#?}",
        import_compiled.warnings
    );
    assert!(
        import_compiled.warnings.iter().any(|d| {
            d.code == "W003"
                && d.message.contains("dntls-epoch")
                && d.message.contains("non_library_time.cddl")
        }),
        "BUG-011 regression: direct `import` of a non-library file with a non-exported reference must also emit W003; got: {:#?}",
        import_compiled.warnings
    );

    let include_path =
        fixture_path("cddl/vectors/project/semantic-errors/include_non_library_warns.cddl");
    let include_compiled = CompiledCDDL::compile(&include_path, None::<&Path>).unwrap();
    assert!(
        include_compiled
            .warnings
            .iter()
            .any(|d| d.code == "W007" && d.message.contains("non_library_time.cddl")),
        "BUG-011 regression: direct `include` of a non-library file must emit W007; got: {:#?}",
        include_compiled.warnings
    );
    assert!(
        include_compiled.warnings.iter().any(|d| {
            d.code == "W003"
                && d.message.contains("dntls-epoch")
                && d.message.contains("non_library_time.cddl")
        }),
        "BUG-011 regression: direct `include` of a non-library file with a non-exported reference must also emit W003; got: {:#?}",
        include_compiled.warnings
    );
}

#[test]
fn bug_004_library_all_root_collision_lints_cleanly() {
    // BUG-004 regression: a consumer that imports two independent
    // CBORK libraries (each declaring its own private root `all`,
    // one plain and one generic) must not emit E013 or E014 for
    // the cross-library `all` collision.  The consumer's direct
    // surface only references each library's exported `foo` /
    // `bar`; the private `all` roots stay scoped to their
    // respective libraries.
    let path = fixture_path("cddl/vectors/project/bugs/bug_004_library_all_root_collision.cddl");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        !compiled.has_errors(),
        "BUG-004 regression: independent library private roots must not produce errors; got: {:#?}",
        compiled.warnings
    );
    assert!(
        !compiled.warnings.iter().any(|d| d.code == "E013"),
        "BUG-004 regression: no E013 (plain-vs-generic collision); got: {:#?}",
        compiled.warnings
    );
    assert!(
        !compiled.warnings.iter().any(|d| d.code == "E014"),
        "BUG-004 regression: no E014 (conflicting definition) for cross-library private roots; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn bug_005_within_renders_multiline_effective_subdiag() {
    // BUG-005 regression: a `.within` failure whose LHS and RHS
    // contain complex nested maps/arrays must render both
    // EFFECTIVE LHS and EFFECTIVE RHS as multiline indented
    // bodies, with symbolic typename dependencies expanded into
    // their concrete definitions.  Single-line collapse of long
    // nested map values is not acceptable.
    let path =
        fixture_path("cddl/vectors/project/bugs/bug_005_within_renders_multiline_effective.cddl");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    let e030 = compiled
        .warnings
        .iter()
        .find(|d| d.code == "E030")
        .expect("BUG-005 fixture must emit an E030 diagnostic");

    // Both EFFECTIVE LHS and EFFECTIVE RHS subdiagnostics must be
    // present and must NOT contain a single long line that
    // collapses a 4-field map into one line.
    let lhs_sub = e030
        .related
        .iter()
        .find(|s| matches!(s.kind, crate::error::SubdiagKind::Lhs))
        .expect("E030 must carry an LHS subdiag");
    let rhs_sub = e030
        .related
        .iter()
        .find(|s| matches!(s.kind, crate::error::SubdiagKind::Rhs))
        .expect("E030 must carry an RHS subdiag");
    let lhs_text = &lhs_sub.snippet;
    let rhs_text = &rhs_sub.snippet;

    // Every entry of the outer `protected: { ... }` map must be on
    // its own line.  Before the fix, the inline renderer collapsed
    // the entire 5-field map into one long single-line value
    // expression.
    assert!(
        lhs_text
            .lines()
            .any(|l| l.trim_start().starts_with("1: 57")),
        "BUG-005 regression: LHS must render `1: 57` on its own line; got:\n{lhs_text}"
    );
    assert!(
        lhs_text
            .lines()
            .any(|l| l.trim_start().starts_with("4: bstr .size 32")),
        "BUG-005 regression: LHS must render `4: bstr .size 32` on its own line; got:\n{lhs_text}"
    );
    assert!(
        rhs_text
            .lines()
            .any(|l| l.trim_start().starts_with("1: 57")),
        "BUG-005 regression: RHS must render `1: 57` on its own line; got:\n{rhs_text}"
    );
    assert!(
        rhs_text
            .lines()
            .any(|l| l.trim_start().starts_with("4: bstr .size 32")),
        "BUG-005 regression: RHS must render `4: bstr .size 32` on its own line; got:\n{rhs_text}"
    );
}

#[test]
fn cbork_export_before_import_emits_e022() {
    // Step 5.12 negative regression: `;@ CBORK: Export` must not
    // skip over an import / include directive comment.
    let path = fixture_path("cddl/vectors/project/semantic-errors/cbork_export_before_import.cddl");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        compiled.warnings.iter().any(|d| d.code == "E022"),
        "Export before import must emit E022; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn cbork_export_in_non_library_emits_e022() {
    // Step 5.12 negative regression: `;@ CBORK: Export` is only
    // valid in a library file.
    let path =
        fixture_path("cddl/vectors/project/semantic-errors/cbork_export_in_non_library.cddl");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        compiled.warnings.iter().any(|d| d.code == "E022"),
        "Export in non-library file must emit E022; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn cbork_unknown_directive_emits_e021() {
    // Step 5.12 negative regression: unknown `;@ CBORK: Thing`
    // directives must produce an E021 diagnostic because they look
    // like active CBORK processing directives.
    let path = fixture_path("cddl/vectors/project/semantic-errors/cbork_unknown_directive.cddl");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        compiled.warnings.iter().any(|d| d.code == "E021"),
        "unknown CBORK directive must emit E021; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn cbork_external_directive_warns_as_w002() {
    // Step 5.12 positive regression: external-namespace `;@ OTHER:
    // ...` directives produce a W002 warning so the user notices
    // their tool annotation was ignored.
    let path = fixture_path("cddl/vectors/project/positive/cbork_external_directive_warns.cddl");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();

    assert!(
        compiled.warnings.iter().any(|d| d.code == "W002"),
        "external directive must produce W002; got: {:#?}",
        compiled.warnings
    );
}

/// Lookup a cache entry by name for assertions.
fn cache_entry(
    cache: &ResolverCache,
    name: &str,
) -> EntryState {
    for (key, state) in cache.iter() {
        if key == name {
            return state.clone();
        }
    }
    panic!("entry {name} not found in cache");
}

fn find_rule_node<'a>(
    nodes: &'a [WrappedNode],
    needle: &str,
) -> Option<&'a WrappedNode> {
    for node in nodes {
        match node {
            WrappedNode::RuleLine { text, children, .. } => {
                if text.contains(needle) {
                    return Some(node);
                }
                if let Some(found) = find_rule_node(children, needle) {
                    return Some(found);
                }
            },
            WrappedNode::Syntax { children, .. } | WrappedNode::Directive { children, .. } => {
                if let Some(found) = find_rule_node(children, needle) {
                    return Some(found);
                }
            },
            WrappedNode::Comment { .. }
            | WrappedNode::ModuleStart { .. }
            | WrappedNode::ModuleEnd { .. } => {},
        }
    }
    None
}

#[test]
fn has_errors_reports_clean_compile_correctly() {
    // Step 6: a clean compile must report `has_errors() == false`.
    // The first rule in a CDDL file is always retained by the
    // reachability pruner, so a single-rule fixture produces a
    // clean compile without unreferenced-definition errors.
    let path = write_temp_file("has_errors_clean.cddl", "a = 1\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    assert!(
        !compiled.has_errors(),
        "a clean compile must report no errors; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn has_errors_reports_undefined_reference_as_error() {
    // Step 6: a reference to an undefined name must surface as
    // a hard error so downstream validators do not treat the
    // resulting tree as a valid schema.
    let path = write_temp_file("has_errors_undef.cddl", "a = missing_symbol\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    assert!(
        compiled.has_errors(),
        "undefined reference must produce at least one error; warnings: {:#?}",
        compiled.warnings
    );
}

#[test]
fn complete_nodes_preserves_origin_for_provenance() {
    // Step 6: the physically complete tree must preserve the
    // source path on every RuleLine so callers can trace rules
    // back to the file they came from.
    let path = write_temp_file("complete_nodes_prov.cddl", "a = 1\nb = a\n");
    let compiled = CompiledCDDL::compile(&path, None::<&Path>).unwrap();
    assert!(
        !compiled.complete_nodes.is_empty(),
        "complete_nodes must not be empty for a well-formed input"
    );
    for node in &compiled.complete_nodes {
        if let WrappedNode::RuleLine { origin, .. } = node {
            assert_eq!(
                origin.source_path, path,
                "every top-level rule must retain its source file path"
            );
        }
    }
}
