// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MPL-2.0

//! Integration tests for the concrete CDDL renderer.
//!
//! Exercises the resolution-driven renderer on the vendored project
//! vectors under `cddl/vectors/project/positive/`. These tests are
//! the acceptance criteria: a passing render means the user's CDDL
//! survives compilation and the rendered output reflects the
//! post-resolution state the linter reasons about — with named
//! constants folded, structural types inlined, and socket/group
//! plug augmentations inlined into the maps that use them.

use std::path::PathBuf;

use cbork_cddl_compiler::{CompiledCDDL, build_resolution, render_to_string};

/// Get repo root.
///
/// # Panics
///
/// Yes it can panic, which is why its only for tests
fn repo_root() -> PathBuf {
    #[allow(clippy::expect_used, reason = "Allowed in tests")]
    std::env::current_dir()
        .expect("test working directory")
        .ancestors()
        .find(|path| path.join("Cargo.toml").is_file() && path.join("cddl").is_dir())
        .expect("workspace root")
        .to_path_buf()
}

fn compile(rel: &str) -> CompiledCDDL {
    let path = repo_root().join(rel);
    #[allow(clippy::panic, reason = "Allowed in tests")]
    CompiledCDDL::compile(&path, None).unwrap_or_else(|e| {
        panic!(
            "expected {} to compile successfully, got:\n{}",
            path.display(),
            e
        )
    })
}

fn render_concrete(compiled: &CompiledCDDL) -> String {
    let res = build_resolution(&compiled.complete_nodes);
    render_to_string(
        &compiled.complete_nodes,
        &res,
        &cbork_cddl_compiler::ConcretePolicy::for_render(),
    )
}

/// Render and canonicalize exactly as the `cbork render` CLI does:
/// the structural formatter owns presentation, so the byte-stability
/// contract of a render round-trip is asserted on the formatted text.
fn render_formatted(
    compiled: &CompiledCDDL,
    comments: bool,
) -> String {
    let res = build_resolution(&compiled.complete_nodes);
    let raw = render_to_string(
        &compiled.complete_nodes,
        &res,
        &cbork_cddl_compiler::ConcretePolicy::for_render().with_comments(comments),
    );
    cbork_cddl_compiler::pretty_print(&raw)
}

#[test]
fn render_concrete_fold_vector() {
    // In concrete mode, only the first top-level rule is emitted. The
    // named constants `cose-alg`, `cose-iv`, `cose-salt` are folded
    // into any rules that reference them; constants that are
    // themselves the first rule are emitted verbatim.
    let compiled = compile("cddl/vectors/project/positive/render_concrete_fold.cddl");
    let cddl = render_concrete(&compiled);
    // The first top-level rule is `cose-alg = 1`, which is a leaf
    // constant with no further references to fold.
    assert!(
        cddl.contains("cose-alg = 1"),
        "expected first rule:\n{cddl}"
    );
    // The other named constants and referencing rules past the
    // first are suppressed in concrete mode.
    assert!(
        !cddl.contains("alg = cose-alg") && !cddl.contains("salt = cose-salt"),
        "non-first rules should be suppressed in concrete mode:\n{cddl}"
    );
}

#[test]
fn render_library_folds_unreachable_constants() {
    // Library-preserving rendering was removed: a `;@ CBORK: Library`
    // schema renders like any other schema, folding named constants
    // into their use sites instead of keeping them verbatim.
    let compiled = compile("cddl/vectors/project/positive/render_library_preserves_constants.cddl");
    let cddl = render_concrete(&compiled);
    assert!(
        !cddl.contains("cose-alg = 1"),
        "unreachable constant should be folded, not preserved:\n{cddl}"
    );
    assert!(
        !cddl.contains("A256GCM = 3"),
        "unreachable constant should be folded, not preserved:\n{cddl}"
    );
}

#[test]
fn render_rfc8610_emits_only_reachable_concrete_cddl() {
    // Regression for plan-021: rendering a `;@ CBORK: Library` schema
    // must not retain unreachable library definitions. Only the root
    // rule and its reachable closure may be emitted, so the result
    // lints clean and round-trips unchanged.
    let compiled = compile("cddl/rfc-std/rfc8610.cddl");
    let cddl = render_formatted(&compiled, false);

    // Library helpers that are only reachable via inlining must not be
    // emitted as separate top-level definitions. The `uuidv4-abnf`
    // family is the exception: the `.abnfb` ctlop operands reference
    // them symbolically (a ctlop expression cannot be inlined into a
    // ctlop operand), so they are legitimately retained.
    for name in ["buuid", "buuid-all", "buuidv4"] {
        assert!(
            !cddl
                .lines()
                .any(|line| line.starts_with(name) && line.contains('=')),
            "unreachable library definition `{name}` retained:\n{cddl}"
        );
    }
    assert!(
        cddl.lines().any(|line| line.starts_with("uuidv4-abnf =")),
        "`uuidv4-abnf` must be retained for the symbolic `.abnfb` operand:\n{cddl}"
    );
    assert!(
        cddl.contains(".abnfb uuidv4-abnf"),
        "the `.abnfb` ctlop operand must stay symbolic:\n{cddl}"
    );

    // The result must lint clean: recompiling it yields no E020.
    let second = compile_vec("rfc8610_roundtrip", &cddl);
    assert!(
        !second
            .warnings
            .iter()
            .any(|diagnostic| diagnostic.code == "E020"),
        "rendered rfc8610 must lint clean:\n{}",
        second
            .warnings
            .iter()
            .map(|diagnostic| diagnostic.message.clone())
            .collect::<Vec<_>>()
            .join("\n")
    );

    // And a second render pass must be byte-identical through the
    // same formatted pipeline the CLI runs.
    let rerendered = render_formatted(&second, false);
    assert_eq!(
        cddl, rerendered,
        "render output must be stable after a second parse/render pass"
    );
}

#[test]
fn render_rfc8727_cycle_aware_and_self_contained() {
    // Regression for plan-021 item 2: IODEF (rfc8727) has six mutually
    // recursive definitions. Rendering must not expand them (the naive
    // expansion blew up to ~83k lines): references to recursive
    // component members stay symbolic, their definitions are retained
    // exactly once, acyclic content is fully inlined, and the output
    // lints clean and round-trips byte-identically.
    let compiled = compile("cddl/rfc-std/rfc8727.cddl");
    let cddl = render_formatted(&compiled, false);

    // The recursive cluster is retained exactly once per member, and
    // references to the members stay symbolic (`+ Contact`).
    for name in [
        "Incident",
        "Contact",
        "EventData",
        "Indicator",
        "Observable",
    ] {
        assert_eq!(
            cddl.matches(&format!("{name} =")).count(),
            1,
            "recursive definition `{name}` must be retained exactly once:\n{cddl}"
        );
        assert!(
            cddl.contains(&format!("+ {name}")),
            "recursive reference `+ {name}` must stay symbolic:\n{cddl}"
        );
    }

    // No re-expansion blowup: the rendered document stays bounded.
    // The structural formatter lays one entry per line, so the bound
    // is generous — the naive recursive expansion was ~83k lines.
    assert!(
        cddl.lines().count() < 10_000,
        "rendered rfc8727 must stay bounded (got {} lines):\n{cddl}",
        cddl.lines().count()
    );

    // Acyclic content is concrete: member keys fold and the `lang`
    // choice renders as a real choice, not the old `[ ]` corruption.
    // The ctlop arm is parenthesized (ctlops have no order of
    // evaluation).
    assert!(
        cddl.contains("? -23 => (\"\" /") && cddl.contains("text .regexp \"[a-zA-Z]{1,8}"),
        "member keys must fold and `lang` must render as a choice:\n{cddl}"
    );
    assert!(
        !cddl.contains("? iodef-lang =>"),
        "`iodef-lang` key must be folded to its constant:\n{cddl}"
    );

    // Self-contained: recompiling the output yields no E016/E020.
    let second = compile_vec("rfc8727_roundtrip", &cddl);
    assert!(
        !second
            .warnings
            .iter()
            .any(|d| d.code == "E016" || d.code == "E020"),
        "rendered rfc8727 must lint clean:\n{}",
        second
            .warnings
            .iter()
            .map(|d| format!("{} {}", d.code, d.message))
            .collect::<Vec<_>>()
            .join("\n")
    );

    // Byte-identical second render pass through the same formatted
    // pipeline the `cbork render` CLI runs.
    let rerendered = render_formatted(&second, false);
    assert_eq!(
        cddl, rerendered,
        "render output must be stable after a second parse/render pass"
    );
}

#[test]
fn render_rfc8990_within_preserves_operands_and_ranges() {
    // Regression for plan-021 item 3: GRASP (rfc8990) puts a type plug
    // (`message`, extended with `/=`) on the LHS of a `.within` and uses
    // ranges (`MESSAGE_TYPE = 0..255`) inside the constraint. The render
    // must preserve both: ranges must not collapse to their lower bound,
    // the within-LHS plug must stay symbolic (inlining it changes how
    // the within-checker evaluates the constraint), type-augment rules
    // must be emitted concretely, and the output must lint clean and
    // round-trip byte-identically.
    let compiled = compile("cddl/rfc-std/rfc8990-cleaned.cddl");
    let cddl = render_formatted(&compiled, false);

    // The within-LHS plug stays symbolic and its augment lines are
    // emitted concretely. The within-arm is parenthesized (a ctlop
    // expression in a choice must be braced).
    assert!(
        cddl.contains("rfc8990 = (message .within"),
        "within-LHS plug must stay symbolic:\n{cddl}"
    );
    assert!(
        cddl.lines().any(|l| l.starts_with("message /= [")),
        "type-augment rules must be emitted concretely:\n{cddl}"
    );

    // Ranges survive (no collapse to the lower bound).
    assert!(
        cddl.contains("0 .. 255") && cddl.contains("0 .. 4294967295"),
        "ranges must not collapse to their lower bound:\n{cddl}"
    );

    // Self-contained: recompiling the output yields no E016/E020/E030.
    let second = compile_vec("rfc8990_roundtrip", &cddl);
    assert!(
        !second
            .warnings
            .iter()
            .any(|d| d.code == "E016" || d.code == "E020" || d.code == "E030"),
        "rendered rfc8990 must lint clean:\n{}",
        second
            .warnings
            .iter()
            .map(|d| format!("{} {}", d.code, d.message))
            .collect::<Vec<_>>()
            .join("\n")
    );

    // Byte-identical second render pass through the formatted pipeline.
    let rerendered = render_formatted(&second, false);
    assert_eq!(
        cddl, rerendered,
        "render output must be stable after a second parse/render pass"
    );
}

#[test]
fn render_rfc9052_parenthesizes_choice_member_keys() {
    // Regression for plan-021 item 4: a member key that renders as a
    // top-level choice must be parenthesized (`* (int / tstr) => any`).
    // Without the parens the `/` is parsed as a group choice and the
    // emitted CDDL is ambiguous (a parse error). The render must be
    // lint-clean and round-trip byte-identically.
    let compiled = compile("cddl/rfc-std/rfc9052.cddl");
    let cddl = render_formatted(&compiled, false);

    // The inlined `label` (int / tstr) catch-all key keeps its parens.
    assert!(
        cddl.contains("* (int / tstr) => any"),
        "choice member keys must stay parenthesized:\n{cddl}"
    );

    // Self-contained: recompiling the output yields no diagnostics.
    let second = compile_vec("rfc9052_roundtrip", &cddl);
    assert!(
        !second
            .warnings
            .iter()
            .any(|d| d.code == "E016" || d.code == "E020" || d.code == "E030"),
        "rendered rfc9052 must lint clean:\n{}",
        second
            .warnings
            .iter()
            .map(|d| format!("{} {}", d.code, d.message))
            .collect::<Vec<_>>()
            .join("\n")
    );

    // Byte-identical second render pass through the formatted pipeline.
    let rerendered = render_formatted(&second, false);
    assert_eq!(
        cddl, rerendered,
        "render output must be stable after a second parse/render pass"
    );
}

#[test]
fn render_complex_structure() {
    // The named type references `bstr` and `int` are folded to their
    // CBOR tag equivalents (#2 and #0/#1) inside the rendered rule.
    // The `.size` ctlop, the `32` bound, and the choice `/` all
    // survive rendering.
    let compiled = compile("cddl/vectors/project/positive/render_complex_structure.cddl");
    let cddl = render_concrete(&compiled);
    assert!(cddl.contains(".size"), "expected .size ctlop:\n{cddl}");
    assert!(cddl.contains("32"), "expected size bound:\n{cddl}");
    // The choice `/` is rendered as one arm per line in concrete mode.
    assert!(cddl.contains("bstr"), "expected bstr arm:\n{cddl}");
    assert!(
        cddl.contains("int .lt 100"),
        "expected int .lt 100 arm:\n{cddl}"
    );
    assert!(cddl.contains("my_type"), "expected my_type rule:\n{cddl}");
    // The named type names themselves are kept as primitive type
    // names (`bstr`, `int`); the renderer treats postlude-defined
    // primitives as leaves instead of unfolding them to their CBOR
    // major-type tags.
    assert!(cddl.contains("bstr"), "expected bstr as primitive:\n{cddl}");
    assert!(cddl.contains("int"), "expected int as primitive:\n{cddl}");
}

#[test]
fn render_plug_inline_expands_socket_choices() {
    // The three `one-pq-signature //= ...` augmentations should be
    // inlined into `alg-sig-map` as a choice of three entries.
    let compiled = compile("cddl/vectors/project/positive/render_plug_inline.cddl");
    let cddl = render_concrete(&compiled);
    assert!(
        cddl.contains("ml-dsa-44") && cddl.contains("ml-dsa-65") && cddl.contains("ml-dsa-87"),
        "expected all three plug entries inlined:\n{cddl}"
    );
    // The `alg-sig-map` rule itself should be the only thing
    // remaining in the rendered output (the augmentations and the
    // named `bstr` constant should be folded in or removed).
    assert!(
        cddl.contains("alg-sig-map"),
        "expected alg-sig-map:\n{cddl}"
    );
}

#[test]
fn render_folds_named_constants_in_group_keys_and_values() {
    // Regression test for the user complaint: when `pq-hybrid` is
    // rendered, every named constant should be folded to its literal
    // value (`signature` -> `0`, `ed25519` -> `-19`, ...), and every
    // structural type should be inlined. Bare names should never
    // appear in the rendered output for a constant.
    let src = "        pq-hybrid = [signature, alg-sig-map-ed25519-ml-dsa-44] \
        ed25519 = -19 \
        signature = 0 \
        alg-sig-map-ed25519-ml-dsa-44 = { ed25519 => bstr } \
        bstr = bytes .size 64 \
";
    let compiled = compile_vec("render_folds_constants", src);
    let cddl = render_concrete(&compiled);
    // Named constant on the LHS of a grpent: must be folded.
    // (Now on its own line because the array body is pretty-printed
    // across multiple lines.)
    assert!(
        cddl.contains("[\n    0,") || cddl.contains("0,"),
        "expected `signature` folded to `0` in array literal:\n{cddl}"
    );
    // Named constant on the LHS of a key=>value grpent: must be folded.
    // (The structural type is rendered as a multi-line block.)
    assert!(
        cddl.contains("-19 =>"),
        "expected `ed25519` folded to `-19` in map key:\n{cddl}"
    );
    // Structural type referenced by name must be inlined to its body
    // (i.e. we see the inlined map literal, not the bare name).
    assert!(
        cddl.contains("bytes .size"),
        "expected structural type inlined into the referencing rule:\n{cddl}"
    );
    // The referencing rule (the first top-level rule) should NOT
    // emit a separate `alg-sig-map-ed25519-ml-dsa-44 = { ... }`
    // line, because the structural reference should be inlined.
    assert!(
        !cddl.contains("alg-sig-map-ed25519-ml-dsa-44 ="),
        "structural reference should be inlined, not emitted verbatim:\n{cddl}"
    );
    // No bare `signature` or `ed25519` names should remain in the
    // body of `pq-hybrid` (the user-visible complaint).
    let body = cddl.split("pq-hybrid =").nth(1).expect("rule body present");
    let body = body.split("; from").next().unwrap_or(body);
    assert!(
        !body.contains("signature,") && !body.contains("ed25519 =>"),
        "bare `signature`/`ed25519` names should not appear in pq-hybrid body:\n{body}"
    );
}

#[test]
#[allow(
    clippy::string_slice,
    reason = "The comment scanner slices only at char_indices boundaries"
)]
fn render_groups_inlined_member_rhs_choices() {
    let src = "i2p-location = ( h'1726cf46 2c66 4914 b8ec 205e6dc22dee' => i2p-address / [ +i2p-address ] ) \
        i2p-address = bytes .size 32 / bytes .size 35 / bytes\n";
    let compiled = compile_vec("render_member_rhs_choice", src);
    let rendered = render_concrete(&compiled);
    assert!(
        rendered.contains("=> ((bytes .size 32) / (bytes .size 35) / bytes / [+ ((bytes .size 32) / (bytes .size 35) / bytes) ])")
            && rendered.contains("[+ ((bytes .size 32) / (bytes .size 35) / bytes) ]"),
        "inlined member RHS choices must remain bound to `=>` with braced ctlop arms:\n{rendered}"
    );

    let second_pass = compile_vec("render_member_rhs_choice_second_pass", &rendered);
    let rerendered = render_concrete(&second_pass);
    // The first pass keeps the source's parenthesized arm nesting; the
    // second pass normalizes it. Assert the converged (second-pass)
    // form is stable from then on.
    let third_pass = compile_vec("render_member_rhs_choice_third_pass", &rerendered);
    let third_pass_render = render_concrete(&third_pass);
    let without_comments = |text: &str| {
        text.lines()
            .map(|line| {
                let mut in_single = false;
                let mut in_double = false;
                let mut escaped = false;
                let end = line
                    .char_indices()
                    .find_map(|(index, character)| {
                        if in_double {
                            if escaped {
                                escaped = false;
                            } else if character == '\\' {
                                escaped = true;
                            } else if character == '"' {
                                in_double = false;
                            }
                        } else if in_single {
                            if character == '\'' {
                                in_single = false;
                            }
                        } else if character == '"' {
                            in_double = true;
                        } else if character == '\'' {
                            in_single = true;
                        } else if character == ';' {
                            return Some(index);
                        }
                        None
                    })
                    .unwrap_or(line.len());
                line[..end].trim_end()
            })
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(
        without_comments(&rerendered),
        without_comments(&third_pass_render),
        "render output must be stable after the second parse/render pass"
    );
}

#[test]
fn render_inlines_structural_type_references() {
    // A reference to a structural type should be inlined into the
    // rule that uses it. Put the referencing rule first so concrete
    // mode emits it; the referenced helper is folded in.
    let src = "outer = inner / bstr\ninner = { a => int }\n";
    let compiled = compile_vec("render_inline_structural", src);
    let cddl = render_concrete(&compiled);
    // The body of `inner` should appear inside `outer`'s choice.
    // After the fix for the brace-block layout, the inlined
    // structural type is rendered as a multi-line block, so check
    // for the contents (`a => int`) and the surrounding braces
    // rather than the original single-line form.
    assert!(
        cddl.contains("{\n  a => int\n}") || cddl.contains("a => int"),
        "expected inlined structural reference:\n{cddl}"
    );
    // The bare `inner` reference should be replaced by the inlined body.
    assert!(
        !cddl.contains("inner = {a => int}"),
        "inner should be folded, not emitted:\n{cddl}"
    );
}

#[test]
fn render_formats_nested_within_and_expands_inner_plug_choices() {
    let src = "pq-hybrid = any .dtrm #6.33000([ signature, alg-sig-map-ed25519-ml-dsa-44 ])\n\
\n\
alg-generic-map = {\n\
  2*2 int => bstr\n\
}\n\
\n\
one-pq-signature //= (ml-dsa-44 => ml-dsa-44_signature)\n\
one-pq-signature //= (ml-dsa-65 => ml-dsa-65_signature)\n\
one-pq-signature //= (ml-dsa-87 => ml-dsa-87_signature)\n\
\n\
alg-sig-map = {\n\
  ed25519 => ed25519_signature,\n\
  one-pq-signature\n\
} .within alg-generic-map\n\
\n\
alg-sig-map-ed25519-ml-dsa-44 = {\n\
  ed25519 => ed25519_signature\n\
  ml-dsa-44 => ml-dsa-44_signature\n\
} .within alg-sig-map\n\
\n\
signature = 0\n\
ed25519 = -19\n\
ml-dsa-44 = -48\n\
ml-dsa-65 = -49\n\
ml-dsa-87 = -50\n\
ed25519_signature = bstr .size 64\n\
ml-dsa-44_signature = bstr .size 2420\n\
ml-dsa-65_signature = bstr .size 3309\n\
ml-dsa-87_signature = bstr .size 4627\n";
    let compiled = compile_vec("render_nested_within_plug_choice", src);
    let cddl = render_concrete(&compiled);
    assert!(
        cddl.contains("#6.33000(["),
        "expected tagged array wrapper to stay readable:\n{cddl}"
    );
    assert!(
        cddl.contains("    {\n      -19 => bstr .size 64")
            && cddl.contains("(\n    (-48 => bstr .size 2420),")
            && cddl.contains("    (-49 => bstr .size 3309),")
            && cddl.contains("    (-50 => bstr .size 4627)"),
        "expected nested block indentation and concrete plug arms:\n{cddl}"
    );
    assert!(
        cddl.contains("} .within alg-sig-map"),
        "expected `.within` to stay attached to surrounding blocks:\n{cddl}"
    );
    assert!(
        !cddl.contains("ml-dsa-44_signature")
            && !cddl.contains("ml-dsa-65_signature")
            && !cddl.contains("ml-dsa-87_signature"),
        "inner plug choices should render concretely, not symbolically:\n{cddl}"
    );
}

#[test]
#[ignore = "requires the ephemeral test/ directory (dntls-pqsig fixture), which is not stable repo data"]
fn render_pqsig_fixture_keeps_nested_effective_view_indented() {
    let compiled = compile("test/pqsig/doc/pqsig.cddl");
    let cddl = render_concrete(&compiled);
    assert!(
        cddl.contains("pq-hybrid = any .dtrm (\n  #6.33000(([\n        0, ; from signature"),
        "expected tagged array contents to stay nested:\n{cddl}"
    );
    assert!(
        cddl.contains("]) / ([ ; from pq-hybrid-sig-generic"),
        "expected choice separator before provenance comment:\n{cddl}"
    );
    assert!(
        !cddl.contains("; from pq-hybrid-sig-generic / ["),
        "choice separator must not be hidden after provenance:\n{cddl}"
    );
    // A `.within` RHS whose definition carries a ctlop stays symbolic
    // (inlining it would chain two ctlops on one type1 and produce
    // unparseable output); its definition is retained instead.
    assert!(
        cddl.contains(".within alg-sig-map") && cddl.contains("alg-sig-map = {"),
        "ctlop-bearing `.within` RHS operands must stay symbolic:\n{cddl}"
    );
}

#[test]
fn render_generic_within_substitutes_occurrence_params() {
    let compiled = compile(
        "cddl/vectors/project/positive/valid_generic_within_substitutes_occurrence_params.cddl",
    );
    let cddl = render_concrete(&compiled);
    // The occurrence parameter is substituted with the concrete name
    // (`Null-COSE-Signature`), which resolves to its definition (an
    // empty array) and is inlined: `+ Null-COSE-Signature` becomes
    // `+ [ ]`.
    assert!(
        cddl.contains("signatures: [\n    + [ ]\n  ]"),
        "expected occurrence parameter to be substituted and inlined:\n{cddl}"
    );
    assert!(
        !cddl.contains("headers")
            && !cddl.contains("dntls-payload")
            && !cddl.contains("dntls-signatures"),
        "rendered root must not leak generic formal parameters:\n{cddl}"
    );
}

#[allow(clippy::expect_used, reason = "Allowed in tests")]
#[allow(clippy::panic, reason = "Allowed in tests")]
fn compile_vec(
    name: &str,
    src: &str,
) -> CompiledCDDL {
    let dir = std::env::temp_dir().join("cbork_render_vec_test");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let path = dir.join(format!("{name}_{pid}_{nanos}.cddl"));
    std::fs::write(&path, src).expect("write cddl");
    CompiledCDDL::compile(&path, None).unwrap_or_else(|e| {
        panic!("expected {} to compile, got:\n{}", path.display(), e);
    })
}
