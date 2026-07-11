// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MPL-2.0

//! Integration tests for the vendored import/include vectors.
//!
//! These tests exercise the compiled AST and resolution behavior against the
//! project vectors under `cddl/vectors/project/`.

use std::path::{Path, PathBuf};

use cbork_cddl_compiler::{CompiledCDDL, Subdiag, SubdiagKind, dump_tree};

/// Get repo root.
///
/// Should only be used in tests.
///
/// # Panics
///
/// Yes it can panic, which is why its only for tests
fn repo_root() -> PathBuf {
    #[allow(clippy::expect_used, reason = "Allowed in tests")]
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn compile_ok(
    rel: &str,
    root_path: Option<&Path>,
) -> String {
    let path = repo_root().join(rel);
    #[allow(clippy::panic, reason = "Allowed in tests")]
    let compiled = CompiledCDDL::compile(&path, root_path).unwrap_or_else(|e| {
        panic!(
            "expected {} to compile successfully, got:\n{}",
            path.display(),
            e
        )
    });
    dump_tree(&compiled)
}

fn compile_err(
    rel: &str,
    root_path: Option<&Path>,
) -> String {
    let path = repo_root().join(rel);
    #[allow(clippy::panic, reason = "Allowed in tests")]
    let err = CompiledCDDL::compile(&path, root_path)
        .err()
        .unwrap_or_else(|| panic!("expected {} to fail", path.display()));
    err.to_string()
}

fn assert_contains_all(
    haystack: &str,
    needles: &[&str],
) {
    for needle in needles {
        assert!(
            haystack.contains(needle),
            "expected dump to contain `{needle}`\n\n{haystack}"
        );
    }
}

#[test]
fn import_std_bare_vector() {
    let dump = compile_ok("cddl/vectors/project/positive/import_std_bare.cddl", None);
    assert_contains_all(&dump, &[
        r#"Directive: Import { filename: WellKnown("rfc9052") }"#,
        "RuleLine: COSE_Key",
        "[Prunable]",
        "RuleLine: cose_key_ref = COSE_Key",
    ]);
}

#[test]
fn import_std_named_vector() {
    let dump = compile_ok("cddl/vectors/project/positive/import_std_named.cddl", None);
    // The named import keeps `COSE_Key` (the selected name, and it is
    // referenced from `named_cose_key`); unselected rules in the same
    // imported subtree that nothing references are pruned.
    assert_contains_all(&dump, &[
        r#"Directive: ImportFrom { names: ["COSE_Key"], filename: WellKnown("rfc9052") }"#,
        "RuleLine: COSE_Key =",
        "RuleLine: named_cose_key = COSE_Key",
    ]);
}

#[test]
fn include_relative_bare_vector() {
    let dump = compile_ok(
        "cddl/vectors/project/positive/include_relative_bare.cddl",
        None,
    );
    assert_contains_all(&dump, &[
        r#"Directive: Include { filename: Relative("./support/relative_chain_level1.cddl") }"#,
        "RuleLine: relative_level1 = relative_level2",
        "RuleLine: relative_level2 = COSE_Key",
        "RuleLine: relative_root = relative_level1",
        "RuleLine: COSE_Key",
    ]);
}

#[test]
fn include_relative_named_vector() {
    let dump = compile_ok(
        "cddl/vectors/project/positive/include_relative_named.cddl",
        None,
    );
    // The named include keeps `keep_me` (the selected name, and it is
    // referenced from `relative_named_root`); `drop_me` is unselected
    // and unreferenced, so it is pruned.
    assert_contains_all(&dump, &[
        r#"Directive: IncludeFrom { names: ["keep_me"], filename: Relative("./support/selective_source.cddl") }"#,
        "RuleLine: keep_me = tstr",
        "RuleLine: relative_named_root = keep_me",
    ]);
}

#[test]
fn include_absolute_repo_root_vector() {
    let repo_root = repo_root();
    let dump = compile_ok(
        "cddl/vectors/project/positive/include_absolute_repo_root.cddl",
        Some(repo_root.as_path()),
    );
    assert_contains_all(&dump, &[
        r#"Directive: Include { filename: Absolute("/cddl/vectors/project/positive/support/absolute_chain_level1.cddl") }"#,
        "RuleLine: absolute_level1 = absolute_level2",
        "RuleLine: absolute_level2 = tstr",
        "RuleLine: absolute_root = absolute_level1",
    ]);
}

#[test]
fn import_absolute_repo_root_vector() {
    let repo_root = repo_root();
    let dump = compile_ok(
        "cddl/vectors/project/positive/import_absolute_repo_root.cddl",
        Some(repo_root.as_path()),
    );
    assert_contains_all(&dump, &[
        r#"Directive: Import { filename: Absolute("/cddl/vectors/project/positive/support/absolute_import_leaf.cddl") }"#,
        "RuleLine: absolute_import_leaf = bstr  [Prunable]",
        "RuleLine: absolute_import_root = absolute_import_leaf",
    ]);
}

#[test]
fn transitive_include_include_import_vector() {
    let dump = compile_ok(
        "cddl/vectors/project/positive/transitive_include_include_import.cddl",
        None,
    );
    assert_contains_all(&dump, &[
        r#"Directive: Include { filename: Relative("./support/transitive_include_level1.cddl") }"#,
        "RuleLine: transitive_include_level1 = transitive_include_level2",
        "RuleLine: transitive_include_level2 = COSE_Key",
        r#"Directive: Import { filename: WellKnown("rfc9052") }"#,
        "RuleLine: COSE_Key",
        "[Prunable]",
        "RuleLine: transitive_include_root = transitive_include_level1",
    ]);
}

#[test]
fn transitive_import_include_include_vector() {
    let dump = compile_ok(
        "cddl/vectors/project/positive/transitive_import_include_include.cddl",
        None,
    );
    assert_contains_all(&dump, &[
        r#"Directive: Import { filename: Relative("./support/transitive_import_level1.cddl") }"#,
        "RuleLine: transitive_import_level1 = transitive_import_level2  [Prunable]",
        "RuleLine: transitive_import_level2 = transitive_import_leaf  [Prunable]",
        "RuleLine: transitive_import_leaf = int  [Prunable]",
        "RuleLine: transitive_import_root = transitive_import_level1",
    ]);
}

#[test]
fn import_relative_named_generic_alias_vector() {
    let dump = compile_ok(
        "cddl/vectors/project/positive/import_relative_named_generic_alias.cddl",
        None,
    );
    assert_contains_all(&dump, &[
        r#"Directive: ImportFromAs { names: ["arg.untagged-argon2id<t>"], filename: Relative("./support/generic_import_leaf.cddl"), alias: "arg" }"#,
        "RuleLine: arg.untagged-argon2id<t> = bstr .cbor t",
        "RuleLine: named_generic_alias_root<t> = arg.untagged-argon2id<t>",
    ]);
}

#[test]
fn nested_alias_transitive_vector() {
    // Tests that nested aliased includes are wrapped only once per
    // alias.  Mid includes leaf as "lf", outer includes mid as "mid".
    //
    // BUG-003 follow-on: the old wrap function double-prefixed an
    // already-aliased reference (`lf.leaf_value` would become
    // `mid.lf.leaf_value` when mid was itself rewrapped into
    // outer).  The corrected wrap leaves already-aliased references
    // alone: leaf's rules are `lf.xxx`, mid's references to
    // `lf.xxx` stay `lf.xxx`, and outer's references to `mid.xxx`
    // stay `mid.xxx`.  The full alias chain is still preserved
    // from each rule's own perspective — `nested_alias_root` in
    // outer reaches `mid.mid_rule` through the chain — but the
    // chain does not stack on every reference.
    //
    // Note: RuleLine text only reflects LHS wrapping; Syntax[typename]
    // nodes carry the fully-wrapped reference names.
    let dump = compile_ok(
        "cddl/vectors/project/positive/nested_alias_outer.cddl",
        None,
    );
    assert_contains_all(&dump, &[
        r#"Directive: IncludeAs { filename: Relative("./support/nested_alias_mid.cddl"), alias: "mid" }"#,
        r#"Directive: IncludeAs { filename: Relative("./nested_alias_leaf.cddl"), alias: "lf" }"#,
        // RuleLine LHS is wrapped by the *immediate* importer only.
        // The bracketed body in the RuleLine text preserves the
        // original source; the wrap actually applied to the body
        // shows up in the nested Syntax[typename] dump below.
        "RuleLine: lf.leaf_value = int",
        "RuleLine: lf.leaf_rule = [ leaf_value ]",
        "RuleLine: mid.mid_rule = lf.leaf_rule",
        "RuleLine: nested_alias_root = mid.mid_rule",
        // Syntax tree references carry each reference's own
        // importer prefix, not the consumer's chain.
        "Syntax[typename]: lf.leaf_value",
        "Syntax[typename]: lf.leaf_rule",
        "Syntax[typename]: mid.mid_rule",
    ]);
}

#[test]
fn import_shared_well_known_outer_vector() {
    // The outer file imports a library that itself imports
    // `rfc9052 as cose`, and then also imports `rfc9052 as cose`
    // directly.  Imports are weak references, so reaching the same
    // well-known in two different scopes must not be treated as a
    // cycle.  The dntls-cose-encrypt regression used to fire E010
    // here; verify the compiled form does not contain a cycle
    // diagnostic.
    let dump = compile_ok(
        "cddl/vectors/project/positive/import_shared_well_known_outer.cddl",
        None,
    );
    assert_contains_all(&dump, &[
        r#"Directive: ImportFromAs { names: ["defs.defs_cose_key_ref"], filename: Relative("./support/import_shared_well_known_lib.cddl"), alias: "defs" }"#,
        r#"Directive: ImportAs { filename: WellKnown("rfc9052"), alias: "cose" }"#,
        // The outer's direct cose import stays at the top level
        "RuleLine: cose.COSE_Key",
        "RuleLine: outer_cose_key_ref = cose.COSE_Key",
    ]);
    // Neither the transitive nor the direct import flagged a cycle
    // even though both touched `rfc9052`.
    assert!(
        !dump.contains("already included (cycle or duplicate)"),
        "expected no cycle diagnostic, got dump:\n{dump}"
    );
}

#[test]
fn import_sibling_well_known_different_alias_vector() {
    // Two sibling imports of the same well-known under different
    // aliases.  The two imports produce two independent subtrees and
    // must not be treated as a cycle: `first.COSE_Key` and
    // `second.COSE_Key` are both reachable.
    let dump = compile_ok(
        "cddl/vectors/project/positive/import_sibling_well_known_different_alias.cddl",
        None,
    );
    assert_contains_all(&dump, &[
        r#"Directive: ImportAs { filename: WellKnown("rfc9052"), alias: "first" }"#,
        r#"Directive: ImportAs { filename: WellKnown("rfc9052"), alias: "second" }"#,
        "RuleLine: first.COSE_Key",
        "RuleLine: second.COSE_Key",
        "RuleLine: first_cose_key_ref = first.COSE_Key",
        "RuleLine: second_cose_key_ref = second.COSE_Key",
    ]);
}

#[test]
fn malformed_import_missing_filename_fails() {
    let err = compile_err(
        "cddl/vectors/project/negative/malformed_import_missing_filename.cddl",
        None,
    );
    assert!(err.contains("directive parse error") || err.contains("missing filename"));
}

#[test]
fn malformed_include_missing_filename_fails() {
    let err = compile_err(
        "cddl/vectors/project/negative/malformed_include_missing_filename.cddl",
        None,
    );
    assert!(err.contains("directive parse error") || err.contains("missing filename"));
}

#[test]
fn recursive_include_a_fails() {
    let err = compile_err(
        "cddl/vectors/project/negative/recursive_include_a.cddl",
        None,
    );
    assert!(err.contains("already included"));
}

#[test]
fn recursive_include_b_fails() {
    let err = compile_err(
        "cddl/vectors/project/negative/recursive_include_b.cddl",
        None,
    );
    assert!(err.contains("already included"));
}

#[test]
fn recursive_import_a_fails() {
    let err = compile_err(
        "cddl/vectors/project/negative/recursive_import_a.cddl",
        None,
    );
    assert!(err.contains("already included"));
}

#[test]
fn recursive_import_b_fails() {
    let err = compile_err(
        "cddl/vectors/project/negative/recursive_import_b.cddl",
        None,
    );
    assert!(err.contains("already included"));
}

#[test]
fn duplicate_include_sibling_fails() {
    // `include` injects directly into the parent's scope, so two
    // sibling `include` directives for the same file would produce
    // duplicate definitions at the same scope.  The resolver must
    // still reject the second include up front, even though the same
    // import under two aliases is allowed.
    let err = compile_err(
        "cddl/vectors/project/negative/duplicate_include_sibling.cddl",
        None,
    );
    assert!(err.contains("already included"));
}

#[test]
fn generic_within_definition_site_scope_vector() {
    // Step 5.7 regression: an imported generic whose `.within` RHS
    // refers to a definition-site alias (here `std.Wrapper`) must
    // resolve through the generic definition's scope, not the
    // consumer's.  The consumer only imports `lib.wrapper`; it never
    // imports `std`.  The lint must pass.
    let dump = compile_ok(
        "cddl/vectors/project/positive/generic_within_definition_site_scope.cddl",
        None,
    );
    // The expanded root must substitute the formal `T` parameter with
    // the concrete argument `uint`, and the `.within` RHS must keep
    // its definition-site alias `std.Wrapper` (not get re-prefixed
    // with the consumer alias `lib`).
    assert_contains_all(&dump, &[
        "RuleLine: root = [uint] .within std.Wrapper",
        "std.Wrapper",
    ]);
    assert!(
        !dump.contains("root = lib.wrapper<uint>"),
        "generic call site should be expanded in the compiled tree:\n{dump}"
    );
    assert!(
        !dump.contains("lib.std.Wrapper"),
        "definition-site alias must not be re-prefixed with the consumer alias:\n{dump}"
    );
    assert!(
        !dump.contains("unresolved name"),
        "expanded `.within` RHS must resolve through the generic's import scope:\n{dump}"
    );
}

#[test]
fn generic_import_retains_private_helper_closure_vector() {
    // Step 5.7 regression: an imported generic whose body
    // references private same-module helpers must keep those
    // helpers reachable after the consumer's import + expansion,
    // and the expanded body references must resolve under the
    // helpers' consumer-aliased keys.
    let dump = compile_ok(
        "cddl/vectors/project/positive/generic_import_retains_private_helper_closure.cddl",
        None,
    );
    assert_contains_all(&dump, &[
        "RuleLine: root = [",
        "lib.lib-private-header",
        "lib.lib-private-values",
        "Concrete-Payload",
    ]);
    assert!(
        !dump.contains("root = lib.Envelope<Concrete-Payload>"),
        "generic call site should be expanded in the compiled tree:\n{dump}"
    );
    assert!(
        !dump.contains("undefined reference"),
        "private helper closure must remain reachable; got dump:\n{dump}"
    );
}

#[test]
fn plain_vs_generic_collision_unreferenced_generic_vector() {
    // Step 5.8 regression: a consumer's strong local plain rule
    // must not collide with an unreferenced weak imported generic
    // helper that the cherry-picked `from` clause selected but
    // the consumer never instantiates.  The reachability pruner
    // drops the generic helper before the collision detector runs.
    let result = std::process::Command::new(env!("CARGO"))
        .args([
            "run",
            "-q",
            "-p",
            "cbork",
            "--",
            "lint",
            "cddl/vectors/project/positive/plain_vs_generic_collision_unreferenced_generic.cddl",
        ])
        .current_dir(repo_root())
        .status()
        .expect("lint invocation");
    assert!(
        result.success(),
        "unreferenced weak imported generic helper must not collide with strong local plain rule; \
         the lint should exit successfully"
    );
}

#[test]
fn same_origin_import_convergence_vector() {
    // Step 5.9 regression: importing the same helper file through
    // two paths that resolve to the same canonical file must not
    // emit a W001 redundant-definition warning.
    let compiled = CompiledCDDL::compile(
        repo_root()
            .join("cddl/vectors/project/positive/same_origin_convergence_effective_name.cddl"),
        None,
    )
    .expect("same-origin convergence fixture must compile");
    assert!(
        !compiled.warnings.iter().any(|d| d.code == "W001"),
        "same-origin import convergence must not emit W001; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn same_origin_well_known_convergence_vector() {
    // Step 5.9 regression: importing the same well-known module
    // through two `from` selectors that share a leaf rule must not
    // emit W001 for the converged rule.
    let compiled = CompiledCDDL::compile(
        repo_root().join("cddl/vectors/project/positive/same_origin_well_known_convergence.cddl"),
        None,
    )
    .expect("same-origin well-known convergence fixture must compile");
    assert!(
        !compiled.warnings.iter().any(|d| d.code == "W001"),
        "same-origin well-known convergence must not emit W001; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn distinct_origin_duplicate_emits_w001_vector() {
    // Step 5.9 negative regression: two different source files
    // defining the same rule with matching content still emit W001.
    let compiled = CompiledCDDL::compile(
        repo_root().join("cddl/vectors/project/semantic-errors/distinct_origin_duplicate.cddl"),
        None,
    )
    .expect("distinct-origin duplicate fixture must compile");
    assert!(
        compiled.warnings.iter().any(|d| d.code == "W001"),
        "distinct-origin duplicate must emit W001; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn distinct_origin_conflict_emits_e014_vector() {
    // Step 5.9 negative regression: two different source files
    // defining the same rule with differing content emit E014.
    let compiled = CompiledCDDL::compile(
        repo_root().join("cddl/vectors/project/semantic-errors/distinct_origin_conflict.cddl"),
        None,
    )
    .expect("distinct-origin conflict fixture must compile");
    assert!(
        compiled.warnings.iter().any(|d| d.code == "E014"),
        "distinct-origin conflict must emit E014; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn bare_group_reference_in_map_vector() {
    // Step 5.10 positive regression: bare group reference inside a
    // map expands the named group's entries; lint must succeed
    // without any cycle or reference diagnostic.
    let compiled = CompiledCDDL::compile(
        repo_root().join("cddl/vectors/project/positive/bare_group_reference_in_map.cddl"),
        None,
    )
    .expect("bare-group-reference-in-map fixture must compile");
    assert!(
        !compiled.warnings.iter().any(|d| d.code == "E030"),
        "bare group reference must not trigger a cycle diagnostic; got: {:#?}",
        compiled.warnings
    );
    assert!(
        !compiled.warnings.iter().any(|d| d.code == "E016"),
        "bare group reference must resolve cleanly; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn unused_selected_import_emits_w004_and_w005_vector() {
    // BUG-002 follow-on regression: a consumer that imports an
    // explicitly selected exported name but never references it
    // must still warn. The `from` clause path is covered by
    // `W004` for the unused directive and `W005` for the unused
    // selected name; the old `W006` for "library export unused by
    // consumer" was the bug.
    let compiled = CompiledCDDL::compile(
        repo_root().join(
            "cddl/vectors/project/semantic-errors/unused_selected_import_emits_w004_w005.cddl",
        ),
        None,
    )
    .expect("unused-selected-import-emits-w004-w005 fixture must compile");
    assert!(
        compiled.warnings.iter().any(|d| d.code == "W004"),
        "consumer that never uses any selected import name must warn W004; got: {:#?}",
        compiled.warnings
    );
    assert!(
        compiled.warnings.iter().any(|d| d.code == "W005"),
        "consumer that never uses a selected import name must warn W005; got: {:#?}",
        compiled.warnings
    );
    // BUG-002 regression: the W006 "unused library export" path
    // must NOT fire, even when the consumer references a sibling
    // export from the same library and leaves the selected one
    // unused.  Export surface is public API, not an obligation
    // for every consumer of the library.
    assert!(
        !compiled.warnings.iter().any(|d| d.code == "W006"),
        "BUG-002 regression: W006 must not fire for unused selected imports; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn include_use_export_consumes_export_vector() {
    // Step 5.12 positive regression: `include` is subject to
    // the same export contract as `import`; referencing only an
    // exported rule from an included library must NOT warn.
    let compiled = CompiledCDDL::compile(
        repo_root().join("cddl/vectors/project/positive/include_use_export_consumes_export.cddl"),
        None,
    )
    .expect("include-use-export-consumes-export fixture must compile");
    assert!(
        !compiled.warnings.iter().any(|d| d.code == "W003"),
        "consuming an exported rule from an included library must not emit W003; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn include_use_export_uses_private_emits_w003_vector() {
    // Step 5.12 negative regression: `include` is subject to
    // the same export contract as `import`; referencing a
    // non-exported private helper from an included library
    // must emit W003.
    let compiled = CompiledCDDL::compile(
        repo_root()
            .join("cddl/vectors/project/semantic-errors/include_use_export_uses_private.cddl"),
        None,
    )
    .expect("include-use-export-uses-private fixture must compile");
    assert!(
        compiled.warnings.iter().any(|d| d.code == "W003"),
        "referencing a non-exported private helper from an included library must emit W003; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn direct_use_export_uses_private_emits_w003_vector() {
    // Step 5.12 negative regression: importing one exported rule
    // from a library but referencing a non-exported private helper
    // in the body must emit W003.
    let compiled = CompiledCDDL::compile(
        repo_root()
            .join("cddl/vectors/project/semantic-errors/direct_use_export_uses_private.cddl"),
        None,
    )
    .expect("direct-use-export-uses-private fixture must compile");
    assert!(
        compiled.warnings.iter().any(|d| d.code == "W003"),
        "referencing a non-exported private helper must emit W003; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn used_import_reference_vector() {
    // Step 5.12 positive regression: a consumer that imports a
    // rule from a support file and references it must not warn
    // about the import being unused.
    let compiled = CompiledCDDL::compile(
        repo_root().join("cddl/vectors/project/positive/used_import_reference.cddl"),
        None,
    )
    .expect("used-import-reference fixture must compile");
    assert!(
        !compiled.warnings.iter().any(|d| d.code == "W004"),
        "consumer that references the imported name must not warn W004; got: {:#?}",
        compiled.warnings
    );
    assert!(
        !compiled.warnings.iter().any(|d| d.code == "W005"),
        "consumer that references the imported name must not warn W005; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn unused_import_emits_w004_and_w005_vector() {
    // Step 5.12 negative regression: a consumer that imports a
    // selected name from a support file but never references it
    // must warn with W004 (whole directive unused) and W005
    // (the selected name itself is unused).
    let compiled = CompiledCDDL::compile(
        repo_root().join("cddl/vectors/project/semantic-errors/unused_import_emits_w004.cddl"),
        None,
    )
    .expect("unused-import-emits-w004 fixture must compile");
    assert!(
        compiled.warnings.iter().any(|d| d.code == "W004"),
        "consumer that never references the import must warn W004; got: {:#?}",
        compiled.warnings
    );
    assert!(
        compiled.warnings.iter().any(|d| d.code == "W005"),
        "consumer that never references the selected import name must warn W005; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn transform_x_enc_within_bstr_vector() {
    // Step 5.11 positive regression: `.x-enc` narrows the carrier.
    let compiled = CompiledCDDL::compile(
        repo_root().join("cddl/vectors/project/positive/transform_x_enc_within_bstr.cddl"),
        None,
    )
    .expect("transform-x-enc-within-bstr fixture must compile");
    assert!(
        !compiled.warnings.iter().any(|d| d.code == "E030"),
        ".x-enc within bare bstr must lint cleanly; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn transform_x_brotli_within_x_compressed_vector() {
    // Step 5.11 positive regression: a named compression algorithm
    // is within the generic `.x-compressed` wrapper.
    let compiled = CompiledCDDL::compile(
        repo_root()
            .join("cddl/vectors/project/positive/transform_x_brotli_within_x_compressed.cddl"),
        None,
    )
    .expect("transform-x-brotli-within-x-compressed fixture must compile");
    assert!(
        !compiled.warnings.iter().any(|d| d.code == "E030"),
        ".x-brotli within .x-compressed must lint cleanly; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn transform_x_compressed_within_x_brotli_vector() {
    // Step 5.11 negative regression: `.x-compressed` is broader
    // than any named compression algorithm.
    let compiled = CompiledCDDL::compile(
        repo_root().join(
            "cddl/vectors/project/semantic-errors/transform_x_compressed_within_x_brotli.cddl",
        ),
        None,
    )
    .expect("transform-x-compressed-within-x-brotli fixture must compile");
    assert!(
        compiled.warnings.iter().any(|d| d.code == "E030"),
        ".x-compressed within .x-brotli must emit E030; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn transform_x_enc_within_x_hash_vector() {
    // Step 5.11 negative regression: `.x-enc` and `.x-hash` belong
    // to distinct transform families.
    let compiled = CompiledCDDL::compile(
        repo_root().join("cddl/vectors/project/semantic-errors/transform_x_enc_within_x_hash.cddl"),
        None,
    )
    .expect("transform-x-enc-within-x-hash fixture must compile");
    assert!(
        compiled.warnings.iter().any(|d| d.code == "E030"),
        ".x-enc within .x-hash must emit E030; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn cbork_export_marks_rule_vector() {
    // Step 5.12 positive regression: `;@ CBORK: Export` marks the
    // next rule as exported and the lint succeeds.
    let compiled = CompiledCDDL::compile(
        repo_root().join("cddl/vectors/project/positive/cbork_export_marks_rule.cddl"),
        None,
    )
    .expect("cbork-export-marks-rule fixture must compile");
    assert!(
        compiled.is_library,
        "fixture must declare itself a library; got: {:#?}",
        compiled.warnings
    );
    assert!(
        compiled.exported_names.contains("public-rule"),
        "public-rule must be exported; got: {:?}",
        compiled.exported_names
    );
}

#[test]
fn cbork_export_at_eof_emits_e022_vector() {
    // Step 5.12 negative regression: `;@ CBORK: Export` at EOF.
    let compiled = CompiledCDDL::compile(
        repo_root().join("cddl/vectors/project/semantic-errors/cbork_export_at_eof.cddl"),
        None,
    )
    .expect("cbork-export-at-eof fixture must compile");
    assert!(
        compiled.warnings.iter().any(|d| d.code == "E022"),
        "Export at EOF must emit E022; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn cbork_unknown_directive_emits_e021_vector() {
    // Step 5.12 negative regression: unknown `;@ CBORK: Thing`.
    let compiled = CompiledCDDL::compile(
        repo_root().join("cddl/vectors/project/semantic-errors/cbork_unknown_directive.cddl"),
        None,
    )
    .expect("cbork-unknown-directive fixture must compile");
    assert!(
        compiled.warnings.iter().any(|d| d.code == "E021"),
        "unknown CBORK directive must emit E021; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn cbork_external_directive_warns_vector() {
    // Step 5.12 positive regression: external-namespace directives
    // emit a W002 warning.
    let compiled = CompiledCDDL::compile(
        repo_root().join("cddl/vectors/project/positive/cbork_external_directive_warns.cddl"),
        None,
    )
    .expect("cbork-external-directive-warns fixture must compile");
    assert!(
        compiled.warnings.iter().any(|d| d.code == "W002"),
        "external directive must emit W002; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn direct_use_export_consumes_export_vector() {
    // Step 5.12 positive regression: importing an exported rule from
    // a library and referencing it directly must NOT warn.
    let compiled = CompiledCDDL::compile(
        repo_root().join("cddl/vectors/project/positive/direct_use_export_consumes_export.cddl"),
        None,
    )
    .expect("direct-use-export-consumes-export fixture must compile");
    assert!(
        !compiled.warnings.iter().any(|d| d.code == "W003"),
        "consuming an exported rule must not emit W003; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn bug_001_cbork_export_before_rule_vector() {
    // BUG-001 regression: a valid `;@ CBORK: Export` immediately
    // before a rule must not emit a false E020 "`;@ CBORK: Export`
    // must be applied to the next rule, not to another directive".
    // The fixture in `cddl/vectors/project/bugs/` mirrors the
    // canonical DNTLS / COSE Library + Export + rule pattern and
    // must compile to a `CompiledCDDL` with the rule recorded as
    // exported and no false-positive E020.
    let compiled = CompiledCDDL::compile(
        repo_root().join("cddl/vectors/project/bugs/cbork_export_before_rule_false_e020.cddl"),
        None,
    )
    .expect("bug-001 fixture must compile");
    assert!(
        compiled.is_library,
        "fixture must declare itself a library; got: {:#?}",
        compiled.warnings
    );
    assert!(
        compiled.exported_names.contains("public-rule"),
        "public-rule must be exported; got: {:?}",
        compiled.exported_names
    );
    assert!(
        !compiled.has_errors(),
        "BUG-001 strict-mode invariant: no hard errors expected; got: {:#?}",
        compiled.warnings
    );
    for diagnostic in &compiled.warnings {
        assert!(
            !(diagnostic.code == "E020"
                && diagnostic
                    .message
                    .contains("must be applied to the next rule")),
            "BUG-001 regression: false E020 emitted for valid export; got: {diagnostic:#?}"
        );
    }
}

#[test]
fn bug_002_whole_library_import_no_w006_vector() {
    // BUG-002 regression: a consumer that imports a whole library
    // (no `from` clause) and references a non-exported private
    // helper from that library must not trigger a W006
    // "unused library export" warning.  A library export is a
    // public API surface, not an obligation for every consumer of
    // the library to reference every exported name.  Before the
    // fix, the unused `public-a` export triggered a false W006.
    let compiled = CompiledCDDL::compile(
        repo_root().join("cddl/vectors/project/bugs/bug_002_whole_library_import_no_w006.cddl"),
        None,
    )
    .expect("bug-002 fixture must compile");
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
    // W003 (cross-file direct-use export) is the legitimate
    // contract for whole-library imports when the consumer
    // references a non-exported symbol.
    assert!(
        compiled.warnings.iter().any(|d| d.code == "W003"),
        "BUG-002 fixture must trigger W003 for the non-exported reference; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn bug_003_alias_generic_helper_closure_vector() {
    // BUG-003 regression: an aliased import chain that goes through
    // a generic helper (consumer -> `middle` -> `a.Wrapper<inner-type>`)
    // must reach `inner-type` without E016 (unresolved typename).
    // The wrap function must:
    //   * strip `<...>` before comparing typenames to local rule names, so that `tagged<t>`
    //     matches `tagged`;
    //   * not re-prefix already-qualified references (`a.Wrapper` must stay `a.Wrapper`);
    //   * not recurse into Directive children, which are already fully resolved by the
    //     importer.
    let compiled = CompiledCDDL::compile(
        repo_root().join("cddl/vectors/project/bugs/bug_003_alias_generic_helper_closure.cddl"),
        None,
    )
    .expect("bug-003 fixture must compile");
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
fn bug_007_within_marks_failing_statement_vector() {
    // BUG-007 regression: the rendered DIFF must mark the failing
    // line as an `Unmatched` subdiag (rendered by the CLI as
    // CONFLICT) and identify the field that failed, not just emit
    // `REASON` notes with `map[i]: ...` text and OK/CONTEXT for
    // every other line.  The failing fixture has a generic wrapper
    // whose nested map entry triggers the `.within` failure deep
    // inside the LHS; the test pins both the conflict marker and
    // the visibility of the failing field.
    let compiled = CompiledCDDL::compile(
        repo_root().join("cddl/vectors/project/bugs/bug_007_within_marks_failing_statement.cddl"),
        None,
    )
    .expect("bug-007 fixture must compile");
    let e030 = compiled
        .warnings
        .iter()
        .find(|d| d.code == "E030")
        .expect("BUG-007 fixture must emit an E030 diagnostic");
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
        "BUG-007 regression: DIFF must mark a failing line as Unmatched; got: {e030:#?}"
    );
    assert!(
        found_unmatched_with_reason.is_some(),
        "BUG-007 regression: an Unmatched subdiag must name the failing field; got: {e030:#?}"
    );
}

#[test]
fn bug_009_bareword_memberkey_lints_cleanly_vector() {
    // BUG-009 regression: a `.within` comparison between a map with
    // bareword member keys and a generic header pattern that admits
    // text keys must succeed without surfacing an E030 for
    // `unresolved name: <bareword>`.
    let compiled = CompiledCDDL::compile(
        repo_root().join("cddl/vectors/project/bugs/bug_009_bareword_memberkey_unresolved.cddl"),
        None,
    )
    .expect("bug-009 fixture must compile");
    assert!(
        !compiled.warnings.iter().any(|d| d.code == "E030"),
        "BUG-009 regression: bareword member keys must not produce `.within` failures; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn bug_010_x_enc_abnfb_carrier_narrowing_vector() {
    // BUG-010 regression: `.x-enc.abnf` and `.x-enc.abnfb` must
    // normalize to the same `ControlOp::XEnc` as the base
    // `.x-enc` operator and trigger the carrier-narrowing
    // short-circuit during `.within` subtype checks.  Without
    // the normalization the textual form falls through to
    // `ControlOp::Other` and the structured subtype collector
    // rejects it against plain `bstr` (the dntls `svcrec`
    // failure shape).  The fixture also covers:
    //   * positive: `(bstr .size 48) .x-enc.abnfb ( ... ) .within bstr` and `.within (bstr /
    //     nil)` must lint cleanly.
    //   * negative carrier: a narrower size constraint must still fail.
    //   * negative transform-family: `.x-enc.abnfb` must not subtype `.x-hash.abnfb` or
    //     `.x-brotli.abnfb`.
    let compiled = CompiledCDDL::compile(
        repo_root().join("cddl/vectors/project/bugs/bug_010_x_enc_abnfb_carrier_narrowing.cddl"),
        None,
    )
    .expect("bug-010 fixture must compile");

    // The positive rules (`positive`, `positive-choice`) must NOT
    // produce any E030: their carrier `(bstr .size 48)` narrows to
    // `bstr` and to `bstr / nil`, so the `.within` check is a no-op.
    let positive_e030_count = compiled
        .warnings
        .iter()
        .filter(|d| d.code == "E030")
        .filter(|d| {
            d.source_file
                .as_deref()
                .and_then(|p| p.to_str())
                .is_some_and(|s| s.ends_with("bug_010_x_enc_abnfb_carrier_narrowing.cddl"))
        })
        .count();
    // The fixture has 3 negative rules and 2 positive rules.  The
    // positives must NOT contribute to the E030 count, so we expect
    // exactly 3 E030s (one per negative rule).
    assert_eq!(
        positive_e030_count, 3,
        "BUG-010 regression: 3 negative cases must each produce an E030 (transform-family or carrier constraint); got {positive_e030_count}: {:#?}",
        compiled.warnings
    );

    // The negative cases must surface E030 failures with the
    // expected transform-family or carrier constraint reason.
    let e030_reasons: Vec<String> = compiled
        .warnings
        .iter()
        .filter(|d| d.code == "E030")
        .filter_map(|d| d.message.lines().nth(1).map(str::to_owned))
        .collect();
    let has_expected_reason = e030_reasons.iter().any(|r| {
        r.contains(".x-enc is not within .x-hash")
            || r.contains(".x-enc is not within a non-encryption transform")
            || r.contains("not subtype of this primitive")
    });
    assert!(
        has_expected_reason,
        "BUG-010 regression: at least one E030 must surface a transform-family or carrier constraint reason; got: {e030_reasons:?}"
    );
}

#[test]
fn bug_004_library_all_root_collision_vector() {
    // BUG-004 regression: a consumer that imports two independent
    // CBORK libraries (each defining a private root `all`, one
    // plain and one generic) must not produce E013 (plain-vs-
    // generic collision) or E014 (conflicting definition) on the
    // cross-library `all` pair.  Independent imported library
    // private roots are not part of the consumer's direct
    // surface; only the consumer's own definitions and the names
    // it directly references participate in collision detection.
    let compiled = CompiledCDDL::compile(
        repo_root().join("cddl/vectors/project/bugs/bug_004_library_all_root_collision.cddl"),
        None,
    )
    .expect("bug-004 fixture must compile");
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
        "BUG-004 regression: no E014 (conflicting definition); got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn bug_005_within_renders_multiline_effective_vector() {
    // BUG-005 regression: a `.within` failure on a generic
    // wrapper must render EFFECTIVE LHS and EFFECTIVE RHS as
    // multiline indented bodies with symbolic typename
    // dependencies (e.g. `Headers`, `Body`) expanded into their
    // concrete map definitions.  The inline renderer used to
    // collapse a 5-field map value into one single-line
    // expression, which the diagnostic renderer then surfaced as
    // `EFFECTIVE LHS` text the user could not read.
    let compiled = CompiledCDDL::compile(
        repo_root()
            .join("cddl/vectors/project/bugs/bug_005_within_renders_multiline_effective.cddl"),
        None,
    )
    .expect("bug-005 fixture must compile");
    let e030 = compiled
        .warnings
        .iter()
        .find(|d| d.code == "E030")
        .expect("BUG-005 fixture must emit an E030 diagnostic");
    let lhs_sub = e030
        .related
        .iter()
        .find(|s| matches!(s.kind, SubdiagKind::Lhs))
        .expect("E030 must carry an LHS subdiag");
    let rhs_sub = e030
        .related
        .iter()
        .find(|s| matches!(s.kind, SubdiagKind::Rhs))
        .expect("E030 must carry an RHS subdiag");
    let lhs_text = &lhs_sub.snippet;
    let rhs_text = &rhs_sub.snippet;
    // Each map entry of the `protected: { ... }` body must
    // occupy its own line in the EFFECTIVE LHS subdiag.  Before
    // the fix, `1: 57` and `4: bstr .size 32` would appear on the
    // same line as the surrounding braces.
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
fn bug_011_non_library_import_emits_w007_vector() {
    // BUG-011 regression: a directly-imported file that is not
    // marked `;@ CBORK: Library` must surface a W007 warning at
    // the import directive.  The same is true for `include`
    // directives.  The warning is distinct from W003 (which is
    // the API-surface violation when the imported file IS a
    // library but the consumer references a non-exported symbol).
    let import_compiled = CompiledCDDL::compile(
        repo_root().join("cddl/vectors/project/semantic-errors/import_non_library_warns.cddl"),
        None,
    )
    .expect("bug-011 import fixture must compile");
    assert!(
        import_compiled
            .warnings
            .iter()
            .any(|d| d.code == "W007" && d.message.contains("non_library_time.cddl")),
        "BUG-011 regression: direct `import` of a non-library file must emit W007; got: {:#?}",
        import_compiled.warnings
    );

    let include_compiled = CompiledCDDL::compile(
        repo_root().join("cddl/vectors/project/semantic-errors/include_non_library_warns.cddl"),
        None,
    )
    .expect("bug-011 include fixture must compile");
    assert!(
        include_compiled
            .warnings
            .iter()
            .any(|d| d.code == "W007" && d.message.contains("non_library_time.cddl")),
        "BUG-011 regression: direct `include` of a non-library file must emit W007; got: {:#?}",
        include_compiled.warnings
    );

    // W003 must fire for the non-exported cross-file reference:
    // the consumer's use of `dntls-epoch` lives outside the
    // imported file's declared export surface, so the cross-file
    // direct-use warning applies even when the imported file is
    // not marked as a `;@ CBORK: Library`.
    assert!(
        import_compiled.warnings.iter().any(|d| {
            d.code == "W003"
                && d.message.contains("dntls-epoch")
                && d.message.contains("non_library_time.cddl")
        }),
        "BUG-011 regression: W003 must fire for a non-exported reference from a non-library import; got: {:#?}",
        import_compiled.warnings
    );
    assert!(
        include_compiled.warnings.iter().any(|d| {
            d.code == "W003"
                && d.message.contains("dntls-epoch")
                && d.message.contains("non_library_time.cddl")
        }),
        "BUG-011 regression: W003 must fire for a non-exported reference from a non-library include; got: {:#?}",
        include_compiled.warnings
    );

    // `cbork lint --strict` must treat W007 as a failure (the
    // exit code is non-zero), so the integration test runs the
    // binary on the import fixture to assert the strict-mode
    // signal is preserved.
    let binary = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("cbork")))
        .or_else(|| {
            let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            Some(manifest_dir.join("../../target/release/cbork"))
        });
    if let Some(bin) = binary.filter(|p| p.exists()) {
        let output = std::process::Command::new(&bin)
            .arg("lint")
            .arg("--strict")
            .arg(
                repo_root()
                    .join("cddl/vectors/project/semantic-errors/import_non_library_warns.cddl"),
            )
            .output();
        if let Ok(out) = output {
            assert!(
                !out.status.success(),
                "BUG-011 regression: strict mode must treat the W007 warning as a failure; status: {:?}, stderr: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}

#[test]
fn bare_generic_template_reference_is_undefined_vector() {
    // A generic definition is a template, not a concrete rule.
    // `Generic<T>` does not define a bare concrete `Generic`, so a
    // root that references `Generic` without arguments must fail
    // even in library mode.
    let compiled = CompiledCDDL::compile(
        repo_root()
            .join("cddl/vectors/project/semantic-errors/bare_generic_template_reference.cddl"),
        None,
    )
    .expect("bare generic template reference fixture must compile");
    assert!(
        compiled.has_errors(),
        "bare generic template reference must be a hard error; got: {:#?}",
        compiled.warnings
    );
    assert!(
        compiled.warnings.iter().any(|d| {
            d.code == "E016"
                && d.message.contains("undefined reference `Generic`")
                && d.message.contains("generic template")
        }),
        "bare generic template reference must emit an explanatory E016; got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn bug_018_imported_generic_within_namespace_resolution_vector() {
    // BUG-018 regression (plan 018): an imported generic whose
    // `.within` RHS uses a name from a namespace imported by the
    // generic's definition site must resolve the RHS through the
    // definition-site scope, not the consumer's.  The
    // `cose.COSE_Encrypt0` reference is supplied by RFC 9052 via the
    // library's own `import rfc9052 as cose` directive; the consumer
    // never imports `cose` itself.
    //
    // The fixture's LHS and RHS shapes differ on purpose: the test
    // asserts two things that together pin the resolution contract:
    //
    // 1. The compiled dump preserves `cose.COSE_Encrypt0` as the `.within` RHS text on the
    //    expanded root rule.  A silent drop or alias rewrite of the RHS would surface here.
    // 2. The subtype check fires with E030 and a structural reason ("Uint not subtype of { 2
    //    entries }"), NOT the `unresolved name` reason.  If the namespace were missing the
    //    diagnostic would carry `unresolved name: cose.COSE_Encrypt0` instead.
    let path = repo_root()
        .join("cddl/vectors/project/bugs/bug_018_imported_generic_within_namespace.cddl");
    let compiled = CompiledCDDL::compile(&path, None).expect("bug-018 fixture must compile");
    let dump = dump_tree(&compiled);

    assert_contains_all(&dump, &[
        "RuleLine: root = [",
        "uint,",
        "ciphertext: bstr / nil",
        "] .within cose.COSE_Encrypt0",
        "[Prunable]",
    ]);
    assert!(
        !dump.contains("unresolved name"),
        "BUG-018 regression: compiled tree must not surface an unresolved-name diagnostic \
         for `cose.COSE_Encrypt0`; got dump:\n{dump}"
    );

    let e030 = compiled.warnings.iter().find(|d| d.code == "E030").expect(
        "BUG-018 regression: a `.within` subtype diagnostic must be produced after the \
                 RHS has resolved (LHS and RHS shapes intentionally differ)",
    );
    assert!(
        !e030.message.contains("unresolved name"),
        "BUG-018 regression: E030 reason must be a structural subtype mismatch, not an \
         unresolved-name diagnostic; got: {e030:#?}"
    );
    assert!(
        e030.message.contains("Uint not subtype of"),
        "BUG-018 regression: E030 reason must mention the structural subtype mismatch between \
         the expanded LHS (`uint`) and the resolved RHS (cose.COSE_Encrypt0's Headers map); \
         got: {e030:#?}"
    );
    // The renderer inlines the resolved `cose.COSE_Encrypt0` body in
    // the EFFECTIVE RHS subdiag.  The presence of `empty_or_serialized_map`
    // and `header_map` (both reached transitively through `cose.Headers`)
    // proves the namespace resolution chain succeeded.
    let rhs_snippet = e030
        .related
        .iter()
        .find(|s| matches!(s.kind, SubdiagKind::Rhs))
        .map(|s| s.snippet.as_str())
        .expect("E030 must carry an RHS subdiag");
    assert!(
        rhs_snippet.contains("empty_or_serialized_map") && rhs_snippet.contains("header_map"),
        "BUG-018 regression: RHS subdiag must be the inlined cose.COSE_Encrypt0 body, proving \
         the namespace resolution succeeded; got snippet:\n{rhs_snippet}"
    );
}

#[test]
fn bug_018_imported_plain_within_namespace_resolution_vector() {
    // BUG-018 contrast fixture: a direct (non-generic) `.within
    // cose.COSE_Encrypt0` from inside an imported library, where the
    // `cose` namespace comes from the library's own internal
    // `import rfc9052 as cose` and the consumer never imports
    // `cose` itself.  The plain (non-generic) shape must already
    // resolve through the library's scope and lint without an
    // unresolved-name diagnostic.  This pins the contrast with the
    // bug-018 generic case: the generic case used to drop the
    // library's `.within` RHS subtree along with the unused
    // template, but the plain case never had that template so it
    // always worked.
    let path =
        repo_root().join("cddl/vectors/project/bugs/bug_018_imported_plain_within_namespace.cddl");
    let compiled = CompiledCDDL::compile(&path, None).expect("plain fixture must compile");
    let dump = dump_tree(&compiled);

    // The plain rule is preserved under the consumer alias, and the
    // library's `.within cose.COSE_Encrypt0` RHS survives intact.
    // The presence of the RHS text proves the namespace resolution
    // chain (`cose` alias from the library's internal `import rfc9052`)
    // succeeded.
    assert_contains_all(&dump, &[
        "RuleLine: encrypt.direct-encrypt = [",
        "ciphertext: bstr / nil",
        "] .within cose.COSE_Encrypt0",
    ]);
    assert!(
        !dump.contains("unresolved name"),
        "BUG-018 contrast: plain imported `.within` RHS must resolve through the library's \
         definition-site import scope; got dump:\n{dump}"
    );
    assert!(
        !compiled
            .warnings
            .iter()
            .any(|d| d.code == "E030" && d.message.contains("unresolved name")),
        "BUG-018 contrast: plain imported `.within` must not emit an E030 \
         unresolved-name diagnostic; got: {:#?}",
        compiled.warnings
    );
    // No `.within` subtype diagnostic should fire either — the
    // definition-site subtree is reachable so the `.within` check
    // sees the resolved `cose.COSE_Encrypt0` RHS, not a missing
    // reference.
    assert!(
        compiled.warnings.iter().all(|d| d.code != "E030"),
        "BUG-018 contrast: plain imported `.within` must not emit any E030 diagnostic; \
         got: {:#?}",
        compiled.warnings
    );
}

#[test]
fn bug_018_unresolved_namespaced_within_still_emits_e030_vector() {
    // BUG-018 negative regression: a generic whose `.within` RHS
    // uses a genuinely missing namespaced rule must still emit an
    // E030 unresolved-name diagnostic.  This guards against the
    // bug-018 fix accidentally silencing all unresolved-name
    // diagnostics on imported-generic `.within` checks.
    let compiled = CompiledCDDL::compile(
        repo_root()
            .join("cddl/vectors/project/semantic-errors/generic_within_unresolved_namespace.cddl"),
        None,
    )
    .expect("bug-018 negative fixture must compile");
    assert!(
        compiled.warnings.iter().any(|d| {
            d.code == "E030"
                && d.message.contains("unresolved name")
                && d.message.contains("cose.COSE_Encrypt0")
        }),
        "BUG-018 negative regression: a generic `.within` RHS that references a \
         genuinely missing namespaced rule must still emit an E030 unresolved-name diagnostic; \
         got: {:#?}",
        compiled.warnings
    );
}
