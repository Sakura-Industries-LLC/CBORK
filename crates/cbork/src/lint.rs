// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: AGPL-3.0-only

//! Lint command execution helpers for the `cbork` CLI.

use std::path::{Path, PathBuf};

use cbork_cddl_compiler::{
    CompiledCDDL, Diagnostic, DiagnosticLevel, DocInternalPolicy, EntryState, MetaData,
    WrappedNode, check_doc_semantics, map_rumdl_diagnostics, scan_doc_blocks,
    transform_to_markdown, validate_doc_source,
};
use cbork_cddl_parser::{try_extract_syntax_error, validate_cddl};
use console::{Emoji, style};

use crate::diagnostics::print_compiler_diagnostics;

/// Check the CDDL file and return the compiled document.
fn check_file(file_path: &Path) -> anyhow::Result<CompiledCDDL> {
    let content = std::fs::read_to_string(file_path)?;
    validate_cddl(&content)?;
    Ok(CompiledCDDL::compile(file_path, None)?)
}

/// Configuration for diagnostic printing.
///
/// Each option is encoded as a bit in the flags field:
///
/// - Bit 0: `print_stats`
/// - Bit 1: `print_summary`
/// - Bit 2: `print_why`
/// - Bit 3: `fail_on_warnings`
#[derive(Debug, Clone)]
pub(crate) struct PrintOptions {
    /// Bitmask encoding the four boolean options.
    pub(crate) flags: u8,
}

/// Bit flag for stats printing option.
pub(crate) const FLAG_STATS: u8 = 1 << 0;
/// Bit flag for summary printing option.
pub(crate) const FLAG_SUMMARY: u8 = 1 << 1;
/// Bit flag for detailed "why" printing option.
pub(crate) const FLAG_WHY: u8 = 1 << 2;
/// Bit flag for fail-on-warnings option.
pub(crate) const FLAG_FAIL_ON_WARNINGS: u8 = 1 << 3;

/// Configuration for the `--doc` documentation linting pass.
#[derive(Debug, Clone)]
pub(crate) struct DocLintOptions {
    /// Whether to run the documentation linting pass.
    pub(crate) enable: bool,
    /// Whether to apply `rumdl` auto-fixes to documentation comments.
    pub(crate) apply_fixes: bool,
    /// Policy for internal (non-exported) definition documentation.
    pub(crate) doc_internal: DocInternalPolicy,
}

impl DocLintOptions {
    /// Returns the default doc-lint options: disabled, no internal
    /// documentation required. This matches the `--doc` CLI flag
    /// default of "off".
    #[cfg(test)]
    #[must_use]
    pub(crate) fn default_off() -> Self {
        Self {
            enable: false,
            apply_fixes: false,
            doc_internal: DocInternalPolicy::default(),
        }
    }
}

/// Combined options for a single lint invocation.
#[derive(Debug, Clone)]
pub(crate) struct LintRunOptions {
    /// Diagnostic-printing options.
    pub(crate) print: PrintOptions,
    /// Documentation linting options.
    pub(crate) doc: DocLintOptions,
    /// Recursive directory scan options.
    pub(crate) scan: RecursiveScanOptions,
}

/// Options controlling how recursive directory scans discover CDDL files.
///
/// `respect_ignore_files` enables `.gitignore`, `.ignore`, Git global
/// excludes, Git exclude files, and parent-directory ignore discovery.
/// `include_hidden` controls whether hidden entries (dotfiles and
/// dot-directories) are surfaced. `.git` directories are always
/// skipped regardless of these settings.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RecursiveScanOptions {
    /// Apply ignore-file filtering during recursive scans.
    pub(crate) respect_ignore_files: bool,
    /// Include hidden entries during recursive scans.
    pub(crate) include_hidden: bool,
}

impl RecursiveScanOptions {
    /// Defaults match the recursive lint CLI: respect ignore files,
    /// skip hidden entries.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn default_off() -> Self {
        Self {
            respect_ignore_files: true,
            include_hidden: false,
        }
    }
}

impl LintRunOptions {
    /// Build a `LintRunOptions` from the bitmask form used by the
    /// existing test surface plus a `DocLintOptions`.
    ///
    /// Scan options default to respecting ignore files and skipping
    /// hidden entries.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn from_flags_and_doc(
        flags: u8,
        doc: DocLintOptions,
    ) -> Self {
        Self {
            print: PrintOptions { flags },
            doc,
            scan: RecursiveScanOptions {
                respect_ignore_files: true,
                include_hidden: false,
            },
        }
    }

    /// Build a `LintRunOptions` from the bitmask form, a
    /// `DocLintOptions`, and explicit `RecursiveScanOptions`.
    #[must_use]
    pub(crate) fn from_flags_doc_and_scan(
        flags: u8,
        doc: DocLintOptions,
        scan: RecursiveScanOptions,
    ) -> Self {
        Self {
            print: PrintOptions { flags },
            doc,
            scan,
        }
    }
}

impl PrintOptions {
    /// Returns `true` if stats printing is enabled.
    pub(crate) fn print_stats(&self) -> bool {
        self.flags & FLAG_STATS != 0
    }

    /// Returns `true` if summary printing is enabled.
    pub(crate) fn print_summary(&self) -> bool {
        self.flags & FLAG_SUMMARY != 0
    }

    /// Returns `true` if detailed "why" printing is enabled.
    pub(crate) fn print_why(&self) -> bool {
        self.flags & FLAG_WHY != 0
    }

    /// Returns `true` if warnings should be treated as failures.
    pub(crate) fn fail_on_warnings(&self) -> bool {
        self.flags & FLAG_FAIL_ON_WARNINGS != 0
    }
}

/// Per-file lint outcome with split diagnostic counters.
///
/// `counts` holds the combined compiler + doc totals, while
/// `compiler_counts` and `doc_counts` keep the breakdown so callers can
/// build either a single combined `Summary:` line (one file, one run)
/// or an aggregate over many files without re-walking diagnostics.
#[allow(
    dead_code,
    reason = "compiler_counts / doc_counts / syntax_error / fatal_error are consumed by the lint tests and reserved for the future --breakdown mode described in plans/enhancements-fixes-plan.md item 001"
)]
#[derive(Debug, Clone)]
pub(crate) struct LintFileResult {
    /// Path that was linted.
    pub(crate) path: PathBuf,
    /// Whether the file passes lint (no errors and no warnings under `--strict`).
    pub(crate) ok: bool,
    /// Combined error/warning counts (compiler + doc).
    pub(crate) counts: DiagnosticCounts,
    /// Compiler-only diagnostic counts.
    pub(crate) compiler_counts: DiagnosticCounts,
    /// Documentation-lint diagnostic counts.
    pub(crate) doc_counts: DiagnosticCounts,
    /// Whether the file failed with a CDDL syntax error.
    pub(crate) syntax_error: bool,
    /// Whether the file failed with a non-syntax, non-compile error.
    pub(crate) fatal_error: bool,
}

/// Recursive-directory aggregate result.
///
/// Carries total file counts, the number that failed, and the combined
/// diagnostic counts so a single "Recursive lint summary:" line can be
/// produced after the walk without re-evaluating anything.
#[derive(Debug, Default, Clone)]
pub(crate) struct LintAggregateResult {
    /// Number of CDDL files processed.
    pub(crate) files: usize,
    /// Number of files whose lint result was not `ok`.
    pub(crate) failed_files: usize,
    /// Total error/warning counts across all files.
    pub(crate) counts: DiagnosticCounts,
}

impl LintAggregateResult {
    /// Number of files that linted cleanly.
    #[must_use]
    pub(crate) fn clean_files(&self) -> usize {
        self.files.saturating_sub(self.failed_files)
    }

    /// Whether the directory traversal had any failed files.
    #[must_use]
    pub(crate) fn ok(&self) -> bool {
        self.failed_files == 0
    }
}

/// How `lint_file` should format per-file output.
///
/// `RecursiveItem` suppresses per-file `Summary:` and stats lines
/// because the owning directory traversal prints a single aggregate
/// line at the end of the walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilePrintMode {
    /// The user passed a single file; print the full single-file layout
    /// (path line, optional summary, optional stats).
    SingleFile,
    /// The user passed a directory and we are about to print one item
    /// inside that directory.
    RecursiveItem,
}

/// Check the CDDL file, prints any errors into the stdout.
pub(crate) fn check_file_with_print(
    file_path: &Path,
    opts: &LintRunOptions,
) -> bool {
    lint_file(file_path, opts, FilePrintMode::SingleFile).ok
}

/// Run the lint pipeline for one file and return its structured result.
///
/// Always prints the per-file compiler and doc diagnostics plus the
/// emoji+path line. In `SingleFile` mode it additionally honors
/// `--summary` and `--stats`. In `RecursiveItem` mode those two
/// per-file details are suppressed; the directory traversal owns
/// the final aggregate summary.
pub(crate) fn lint_file(
    file_path: &Path,
    opts: &LintRunOptions,
    mode: FilePrintMode,
) -> LintFileResult {
    match check_file(file_path) {
        Ok(compiled) => lint_file_compiled(file_path, opts, mode, &compiled),
        Err(e) => lint_file_failed(file_path, opts, mode, &e),
    }
}

/// Compile-succeeded branch: print diagnostics, accumulate counts, and
/// emit the success path output. Includes the doc-lint pass when
/// `--doc` is set.
fn lint_file_compiled(
    file_path: &Path,
    opts: &LintRunOptions,
    mode: FilePrintMode,
    compiled: &CompiledCDDL,
) -> LintFileResult {
    print_compiler_diagnostics(file_path, &compiled.warnings, opts.print.print_why());
    let compiler_counts = DiagnosticCounts::from_diagnostics(&compiled.warnings);

    let doc_diagnostics = if opts.doc.enable {
        run_doc_lint(file_path, compiled, opts)
    } else {
        Vec::new()
    };
    print_compiler_diagnostics(file_path, &doc_diagnostics, opts.print.print_why());
    let doc_counts = DiagnosticCounts::from_diagnostics(&doc_diagnostics);

    let counts = compiler_counts.combined_with(doc_counts);
    let ok = !compiler_counts.has_errors()
        && !doc_counts.has_errors()
        && (!opts.print.fail_on_warnings() || counts.warnings == 0);

    let result = LintFileResult {
        path: file_path.to_path_buf(),
        ok,
        counts,
        compiler_counts,
        doc_counts,
        syntax_error: false,
        fatal_error: false,
    };

    print_path_line(&result);

    if mode == FilePrintMode::SingleFile {
        if opts.print.print_summary() {
            result.counts.print();
        }
        if opts.print.print_stats() {
            LintStats::from_compiled(compiled, counts).print();
        }
    }

    result
}

/// Top-level failure dispatch: routes to the compile-error, syntax-error,
/// or fatal-error branch based on the error type.
fn lint_file_failed(
    file_path: &Path,
    opts: &LintRunOptions,
    mode: FilePrintMode,
    e: &anyhow::Error,
) -> LintFileResult {
    if let Some(error) = e.downcast_ref::<cbork_cddl_compiler::CompileError>() {
        return lint_compile_error(file_path, opts, mode, error);
    }
    if try_extract_syntax_error(e.as_ref()).is_some() {
        return lint_syntax_error(file_path, e);
    }
    lint_fatal_error(file_path, e)
}

/// Compile-error branch: the CDDL parsed but failed semantic checks.
///
/// Per the design, the doc-lint pass is skipped because the CDDL is
/// not in a state where doc comments are meaningful.
fn lint_compile_error(
    file_path: &Path,
    opts: &LintRunOptions,
    mode: FilePrintMode,
    error: &cbork_cddl_compiler::CompileError,
) -> LintFileResult {
    print_compiler_diagnostics(file_path, &error.diagnostics, opts.print.print_why());
    let compiler_counts = DiagnosticCounts::from_diagnostics(&error.diagnostics);
    let ok = !compiler_counts.has_errors()
        && (!opts.print.fail_on_warnings() || compiler_counts.warnings == 0);
    let result = LintFileResult {
        path: file_path.to_path_buf(),
        ok,
        counts: compiler_counts,
        compiler_counts,
        doc_counts: DiagnosticCounts::default(),
        syntax_error: false,
        fatal_error: false,
    };
    print_path_line(&result);
    if mode == FilePrintMode::SingleFile {
        if opts.print.print_summary() {
            result.counts.print();
        }
        if opts.print.print_stats() {
            LintStats::from_error(compiler_counts).print();
        }
    }
    result
}

/// Syntax-error branch: pest reported a parse failure.
///
/// The aggregate contribution is exactly one error per file.
fn lint_syntax_error(
    file_path: &Path,
    e: &anyhow::Error,
) -> LintFileResult {
    println!(
        "{} {} (1 syntax error):
{}",
        Emoji::new("🚨", "Syntax error"),
        style(file_path.display()).bold(),
        style(e).red()
    );
    LintFileResult {
        path: file_path.to_path_buf(),
        ok: false,
        counts: DiagnosticCounts {
            errors: 1,
            warnings: 0,
        },
        compiler_counts: DiagnosticCounts::default(),
        doc_counts: DiagnosticCounts::default(),
        syntax_error: true,
        fatal_error: false,
    }
}

/// Fatal-error branch: any non-compile, non-syntax failure (I/O, etc.).
///
/// The aggregate contribution is exactly one error per file.
fn lint_fatal_error(
    file_path: &Path,
    e: &anyhow::Error,
) -> LintFileResult {
    println!(
        "{} {}:
{}",
        Emoji::new("🚨", "Errors"),
        file_path.display(),
        style(e).red()
    );
    LintFileResult {
        path: file_path.to_path_buf(),
        ok: false,
        counts: DiagnosticCounts {
            errors: 1,
            warnings: 0,
        },
        compiler_counts: DiagnosticCounts::default(),
        doc_counts: DiagnosticCounts::default(),
        syntax_error: false,
        fatal_error: true,
    }
}

/// Print the success/error emoji + file path line for one lint result.
fn print_path_line(result: &LintFileResult) {
    let (emoji, fallback) = if result.ok {
        ("✅", "Success")
    } else {
        ("🚨", "Errors")
    };
    println!(
        "{} {}",
        Emoji::new(emoji, fallback),
        style(result.path.display()).bold()
    );
}

/// Run the `--doc` documentation linting pass against the compiled
/// source. Returns the diagnostics emitted by step 6 (transform
/// safety), step 7 (rumdl integration mapped back through step 8),
/// and step 9 (semantic checks).
///
/// `--fix --doc` is wired through `apply_fixes` so that `rumdl` runs
/// its fix pass in memory; the conservative reverse transform that
/// writes a fixed CDDL file to disk is step 11 of the plan.
fn run_doc_lint(
    file_path: &Path,
    compiled: &CompiledCDDL,
    opts: &LintRunOptions,
) -> Vec<Diagnostic> {
    let source_path: &Path = file_path;
    let Ok(source_text) = std::fs::read_to_string(source_path) else {
        return Vec::new();
    };

    // Step 6: safety validation. Errors here stop the pipeline so the
    // remaining steps do not produce misleading diagnostics.
    let safety = validate_doc_source(&source_text);
    let mut diagnostics = safety.diagnostics;
    if diagnostics
        .iter()
        .any(cbork_cddl_compiler::Diagnostic::is_error)
    {
        return diagnostics;
    }

    // Doc-marker spacing check (W036). Only fires under `--doc`.
    // Delegates to the helper defined just below so this top-level
    // dispatch stays under the clippy line budget.
    diagnostics.extend(spacing_diagnostics(compiled, &source_text));

    // Step 5: transform to synthetic Markdown.
    let synthetic = transform_to_markdown(&source_text);

    // Step 7: run rumdl. The config is discovered relative to the
    // CDDL source path, not the process current directory.
    let rumdl_warnings =
        match cbork_cddl_compiler::lint_synthetic_markdown(&synthetic, source_path, None) {
            Ok(run) => run.warnings,
            Err(e) => {
                diagnostics.push(Diagnostic {
                    code: "W031",
                    level: DiagnosticLevel::Warning,
                    message: format!("rumdl integration failed: {e}"),
                    source_file: Some(source_path.to_path_buf()),
                    span: None,
                    previous_origin: None,
                    related: Vec::new(),
                });
                return diagnostics;
            },
        };

    if opts.doc.apply_fixes {
        // Step 10: rumdl fix pass in memory.
        let fixed_synthetic =
            match cbork_cddl_compiler::apply_rumdl_fixes(&synthetic, &rumdl_warnings) {
                Ok(fixed) => fixed,
                Err(e) => {
                    diagnostics.push(Diagnostic {
                        code: "W033",
                        level: DiagnosticLevel::Warning,
                        message: format!("rumdl fix apply failed: {e}"),
                        source_file: Some(source_path.to_path_buf()),
                        span: None,
                        previous_origin: None,
                        related: Vec::new(),
                    });
                    return diagnostics;
                },
            };

        // Step 11: reverse the transform back to CDDL and write to
        // disk.  The reverse transform rejects the fix if the splice
        // markers were damaged.  Byte-for-byte preservation of
        // non-doc CDDL spans is guaranteed by the splice-marker
        // restoration.
        match cbork_cddl_compiler::reverse_transform(&fixed_synthetic, &source_text, &synthetic) {
            Ok(reconstructed) => {
                if std::fs::read(source_path)
                    .is_ok_and(|current| current == reconstructed.as_bytes())
                {
                    return collect_doc_diagnostics(source_path, compiled, opts, &source_text);
                }
                if let Err(e) = std::fs::write(source_path, &reconstructed) {
                    diagnostics.push(Diagnostic {
                        code: "W034",
                        level: DiagnosticLevel::Warning,
                        message: format!("failed to write fixed CDDL: {e}"),
                        source_file: Some(source_path.to_path_buf()),
                        span: None,
                        previous_origin: None,
                        related: Vec::new(),
                    });
                } else {
                    return collect_post_fix_diagnostics(source_path, opts, &reconstructed);
                }
            },
            Err(e) => {
                diagnostics.push(Diagnostic {
                    code: "E035",
                    level: DiagnosticLevel::Error,
                    message: format!("reverse transform rejected: {e}"),
                    source_file: Some(source_path.to_path_buf()),
                    span: None,
                    previous_origin: None,
                    related: Vec::new(),
                });
            },
        }
    }

    // Step 8: map the rumdl diagnostics back to the original CDDL
    // source coordinates.
    let mapped = map_rumdl_diagnostics(rumdl_warnings, &synthetic, &source_text, source_path);
    diagnostics.extend(mapped.diagnostics);

    // Step 9: run the semantic doc-lint pass. The "exported" set
    // comes from the real `;@ CBORK: Export` directive model
    // (compiled.exported_names) rather than the old extern declarations.
    let scan = scan_doc_blocks(&source_text);
    let semantics = check_doc_semantics(
        &source_text,
        source_path,
        &compiled.user_nodes,
        &scan,
        &cbork_cddl_compiler::DocSemanticsConfig {
            doc_internal: opts.doc.doc_internal,
            exported_names: compiled.exported_names.clone(),
        },
    );
    diagnostics.extend(semantics.diagnostics);

    diagnostics
}

/// Recompile the just-written fixed file and collect post-fix doc diagnostics.
fn collect_post_fix_diagnostics(
    source_path: &Path,
    opts: &LintRunOptions,
    reconstructed: &str,
) -> Vec<Diagnostic> {
    let wrote = fixed_cddl_written_diagnostic(source_path);
    let fresh_compiled = match check_file(source_path) {
        Ok(compiled) => compiled,
        Err(e) => return vec![wrote, post_fix_recompile_failed(source_path, &e)],
    };
    let mut diagnostics =
        collect_doc_diagnostics(source_path, &fresh_compiled, opts, reconstructed);
    diagnostics.insert(0, wrote);
    diagnostics
}

/// Build the successful `--doc --fix` write diagnostic.
fn fixed_cddl_written_diagnostic(source_path: &Path) -> Diagnostic {
    Diagnostic {
        code: "W035",
        level: DiagnosticLevel::Warning,
        message: "--doc --fix wrote the fixed CDDL back to disk".to_owned(),
        source_file: Some(source_path.to_path_buf()),
        span: None,
        previous_origin: None,
        related: Vec::new(),
    }
}

/// Build the warning used when a just-written fixed file cannot be
/// compiled for post-fix diagnostic reporting.
fn post_fix_recompile_failed(
    source_path: &Path,
    error: &anyhow::Error,
) -> Diagnostic {
    Diagnostic {
        code: "W037",
        level: DiagnosticLevel::Warning,
        message: format!("failed to recompile fixed CDDL for post-fix diagnostics: {error}"),
        source_file: Some(source_path.to_path_buf()),
        span: None,
        previous_origin: None,
        related: Vec::new(),
    }
}

/// Collect documentation diagnostics for an already-loaded source text
/// without applying fixes.
fn collect_doc_diagnostics(
    source_path: &Path,
    compiled: &CompiledCDDL,
    opts: &LintRunOptions,
    source_text: &str,
) -> Vec<Diagnostic> {
    let safety = validate_doc_source(source_text);
    let mut diagnostics = safety.diagnostics;
    if diagnostics
        .iter()
        .any(cbork_cddl_compiler::Diagnostic::is_error)
    {
        return diagnostics;
    }

    diagnostics.extend(spacing_diagnostics(compiled, source_text));

    let synthetic = transform_to_markdown(source_text);
    let rumdl_warnings =
        match cbork_cddl_compiler::lint_synthetic_markdown(&synthetic, source_path, None) {
            Ok(run) => run.warnings,
            Err(e) => {
                diagnostics.push(Diagnostic {
                    code: "W031",
                    level: DiagnosticLevel::Warning,
                    message: format!("rumdl integration failed: {e}"),
                    source_file: Some(source_path.to_path_buf()),
                    span: None,
                    previous_origin: None,
                    related: Vec::new(),
                });
                return diagnostics;
            },
        };

    let mapped = map_rumdl_diagnostics(rumdl_warnings, &synthetic, source_text, source_path);
    diagnostics.extend(mapped.diagnostics);

    let scan = scan_doc_blocks(source_text);
    let semantics = check_doc_semantics(
        source_text,
        source_path,
        &compiled.user_nodes,
        &scan,
        &cbork_cddl_compiler::DocSemanticsConfig {
            doc_internal: opts.doc.doc_internal,
            exported_names: compiled.exported_names.clone(),
        },
    );
    diagnostics.extend(semantics.diagnostics);

    diagnostics
}

/// Collect W036 spacing diagnostics for every standalone `;!`
/// documentation comment whose marker is not followed by a single
/// space before any non-whitespace text. A bare `;!` line, or `;!`
/// followed only by whitespace, is a blank line inside the doc block
/// and is valid. Trailing `;!` comments are out of scope here (the
/// W030 marker-misuse rule owns them). This check only fires under
/// `--doc` because the user's intent is to be opinionated about doc
/// comment shape, not about general CDDL source comments.
fn spacing_diagnostics(
    compiled: &CompiledCDDL,
    source_text: &str,
) -> Vec<Diagnostic> {
    cbork_cddl_compiler::collect_marker_spacing_issues(&compiled.user_nodes, source_text)
}

/// CDDL file extension. Filter directory files to apply the linter only on
/// the CDDL files.
const CDDL_FILE_EXTENSION: &str = "cddl";

/// Discover CDDL files under `root` using the `ignore` crate.
///
/// Honors `.gitignore`, nested `.gitignore`, `.ignore`, Git global
/// excludes, Git exclude files, and parent-directory ignore discovery
/// when `scan.respect_ignore_files` is `true`. Hidden entries are
/// skipped unless `scan.include_hidden` is `true`. `.git` directories
/// are always filtered out. The result is deterministically ordered.
fn discover_cddl_files(
    root: &Path,
    scan: RecursiveScanOptions,
) -> anyhow::Result<Vec<PathBuf>> {
    let mut builder = ignore::WalkBuilder::new(root);
    builder
        .hidden(!scan.include_hidden)
        .ignore(scan.respect_ignore_files)
        .git_ignore(scan.respect_ignore_files)
        .git_global(scan.respect_ignore_files)
        .git_exclude(scan.respect_ignore_files)
        .parents(scan.respect_ignore_files)
        .sort_by_file_path(std::cmp::Ord::cmp)
        .filter_entry(|entry| !is_git_dir(entry.path()));

    let mut files = Vec::new();
    for entry in builder.build() {
        let entry = entry?;
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let path = entry.into_path();
        if path
            .extension()
            .is_some_and(|ext| ext.eq(CDDL_FILE_EXTENSION))
        {
            files.push(path);
        }
    }
    Ok(files)
}

/// Returns `true` when `path` is (or is contained in) a `.git` directory.
fn is_git_dir(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str() == ".git")
}

/// Check the directory tree (compatibility wrapper).
///
/// Returns `true` iff every `.cddl` file in the tree lints cleanly.
/// The aggregate summary line is only printed when `--summary` is
/// passed via `opts`.
pub(crate) fn check_dir_with_print(
    dir_path: &Path,
    opts: &LintRunOptions,
) -> bool {
    lint_dir(dir_path, opts).ok()
}

/// Walk a directory tree, lint each `.cddl` file, and accumulate totals.
///
/// Per-file output is printed in [`FilePrintMode::RecursiveItem`] mode
/// (one emoji+path line each, no per-file summary or stats). If
/// `--summary` was requested, a single "Recursive lint summary:" line is
/// emitted at the very end of the walk so users see one clear total
/// instead of one summary per file.
pub(crate) fn lint_dir(
    dir_path: &Path,
    opts: &LintRunOptions,
) -> LintAggregateResult {
    let mut aggregate = LintAggregateResult::default();
    match discover_cddl_files(dir_path, opts.scan) {
        Ok(files) => {
            for path in files {
                let result = lint_file(&path, opts, FilePrintMode::RecursiveItem);
                aggregate.files = aggregate.files.saturating_add(1);
                if !result.ok {
                    aggregate.failed_files = aggregate.failed_files.saturating_add(1);
                }
                aggregate.counts = aggregate.counts.combined_with(result.counts);
            }
        },
        Err(e) => {
            println!(
                "{} {}:
{}",
                Emoji::new("🚨", "Errors"),
                dir_path.display(),
                style(&e).red()
            );
            aggregate.failed_files = aggregate.failed_files.saturating_add(1);
            aggregate.counts.errors = aggregate.counts.errors.saturating_add(1);
        },
    }
    if opts.print.print_summary() {
        println!(
            "{}",
            style(format!(
                "Recursive lint summary: {} file(s), {} failed, {} clean, {} error(s), {} warning(s)",
                aggregate.files,
                aggregate.failed_files,
                aggregate.clean_files(),
                aggregate.counts.errors,
                aggregate.counts.warnings
            ))
            .dim()
        );
    }
    aggregate
}
/// Summary statistics emitted for a lint run.
#[derive(Debug, Default)]
struct LintStats {
    /// Total AST node count.
    ast_nodes: usize,
    /// Rule definition count.
    rules: usize,
    /// Literal syntax node count.
    literals: usize,
    /// Comment node count.
    comments: usize,
    /// Include/import directive count.
    directives: usize,
    /// Control operator syntax node count.
    ctlops: usize,
    /// Definitions injected from the standard postlude.
    postlude_injected: usize,
    /// Resolver-cache entry count.
    cache_entries: usize,
    /// Resolved concrete literal/type constants.
    resolved_constants: u64,
    /// Pruned resolver-cache entries.
    pruned_entries: u64,
    /// Text literal constants.
    text_constants: usize,
    /// Byte literal constants.
    byte_constants: usize,
    /// Numeric literal constants.
    numeric_constants: usize,
    /// Compiler warnings collected during lint.
    warnings: usize,
    /// Compiler errors collected during lint.
    errors: usize,
}

impl LintStats {
    /// Build lint statistics from a compiled document.
    #[must_use]
    fn from_compiled(
        compiled: &CompiledCDDL,
        counts: DiagnosticCounts,
    ) -> Self {
        let mut stats = Self {
            cache_entries: compiled.resolved_types.len(),
            resolved_constants: compiled.resolved_types.cnt_resolved(),
            pruned_entries: compiled.resolved_types.cnt_pruned(),
            warnings: counts.warnings,
            errors: counts.errors,
            ..Self::default()
        };

        let nodes = if compiled.complete_nodes.is_empty() {
            compiled.user_nodes.as_slice()
        } else {
            compiled.complete_nodes.as_slice()
        };

        for node in nodes {
            stats.visit_node(node);
        }

        for (_, state) in compiled.resolved_types.iter() {
            match state {
                EntryState::Text(_) | EntryState::Regex(_) => {
                    stats.text_constants = stats.text_constants.wrapping_add(1);
                },
                EntryState::Bytes(_)
                | EntryState::Abnf(_)
                | EntryState::EncAbnf(_)
                | EntryState::HashAbnf(_)
                | EntryState::CompressionAbnf { .. } => {
                    stats.byte_constants = stats.byte_constants.wrapping_add(1);
                },
                EntryState::Integer(_)
                | EntryState::Float(_)
                | EntryState::RangeInt { .. }
                | EntryState::RangeFloat { .. } => {
                    stats.numeric_constants = stats.numeric_constants.wrapping_add(1);
                },
                EntryState::Unresolved | EntryState::Pruned => {},
            }
        }

        stats
    }

    /// Build minimal lint statistics from a compile error.
    #[must_use]
    fn from_error(counts: DiagnosticCounts) -> Self {
        Self {
            warnings: counts.warnings,
            errors: counts.errors,
            ..Self::default()
        }
    }

    /// Recursively collect node-level statistics.
    fn visit_node(
        &mut self,
        node: &WrappedNode,
    ) {
        self.ast_nodes = self.ast_nodes.wrapping_add(1);

        if node.metadata().contains(&MetaData::StandardPostlude)
            && matches!(node, WrappedNode::RuleLine { .. })
        {
            self.postlude_injected = self.postlude_injected.wrapping_add(1);
        }

        match node {
            WrappedNode::RuleLine { children, .. } => {
                self.rules = self.rules.wrapping_add(1);
                for child in children {
                    self.visit_node(child);
                }
            },
            WrappedNode::Comment { .. } => {
                self.comments = self.comments.wrapping_add(1);
            },
            WrappedNode::Syntax { rule, children, .. } => {
                match rule.as_str() {
                    "value" => self.literals = self.literals.wrapping_add(1),
                    "ctlop" => self.ctlops = self.ctlops.wrapping_add(1),
                    _ => {},
                }
                for child in children {
                    self.visit_node(child);
                }
            },
            WrappedNode::Directive { children, .. } => {
                self.directives = self.directives.wrapping_add(1);
                for child in children {
                    self.visit_node(child);
                }
            },
            WrappedNode::ModuleStart { .. } | WrappedNode::ModuleEnd { .. } => {},
        }
    }

    /// Print the successful lint statistics table.
    fn print(&self) {
        println!("{}", style("Lint statistics").bold().cyan());
        print_stat_table(&[
            ("Errors", self.errors.to_string()),
            ("Warnings", self.warnings.to_string()),
            ("Rules", self.rules.to_string()),
            ("Literals", self.literals.to_string()),
            ("Text constants", self.text_constants.to_string()),
            ("Byte constants", self.byte_constants.to_string()),
            ("Numeric constants", self.numeric_constants.to_string()),
            ("Control operators", self.ctlops.to_string()),
            ("Comments", self.comments.to_string()),
            ("Directives", self.directives.to_string()),
            ("Postlude injected", self.postlude_injected.to_string()),
            ("AST nodes", self.ast_nodes.to_string()),
            ("Cache entries", self.cache_entries.to_string()),
            ("Resolved constants", self.resolved_constants.to_string()),
            ("Pruned entries", self.pruned_entries.to_string()),
        ]);
    }
}

/// Error/warning totals for a lint run.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct DiagnosticCounts {
    /// Number of error diagnostics.
    pub(crate) errors: usize,
    /// Number of warning diagnostics.
    pub(crate) warnings: usize,
}

impl DiagnosticCounts {
    /// Count diagnostics by severity.
    #[must_use]
    fn from_diagnostics(diagnostics: &[Diagnostic]) -> Self {
        let mut counts = Self::default();
        for diagnostic in diagnostics {
            match diagnostic.level {
                DiagnosticLevel::Error => counts.errors = counts.errors.wrapping_add(1),
                DiagnosticLevel::Warning => counts.warnings = counts.warnings.wrapping_add(1),
            }
        }
        counts
    }

    /// Returns `true` if any error count is non-zero.
    #[must_use]
    pub(crate) fn has_errors(&self) -> bool {
        self.errors > 0
    }

    /// Combine two counts using saturating addition per field.
    ///
    /// Used in `lint_file` (compiler + doc) and `lint_dir_inner`
    /// (per-file + running aggregate) without going through `+=` so
    /// the `arithmetic_side_effects` clippy lint stays quiet.
    #[must_use]
    pub(crate) fn combined_with(
        self,
        other: Self,
    ) -> Self {
        Self {
            errors: self.errors.saturating_add(other.errors),
            warnings: self.warnings.saturating_add(other.warnings),
        }
    }

    /// Print the compact summary line.
    fn print(self) {
        println!(
            "{}",
            style(format!(
                "Summary: {} error(s), {} warning(s)",
                self.errors, self.warnings
            ))
            .dim()
        );
    }
}

/// Print a two-column statistics table.
fn print_stat_table(rows: &[(&str, String)]) {
    let label_width = rows
        .iter()
        .map(|(label, _)| label.len())
        .max()
        .unwrap_or(0)
        .max("Metric".len());
    let value_width = rows
        .iter()
        .map(|(_, value)| value.len())
        .max()
        .unwrap_or(0)
        .max("Value".len());
    let border = format!(
        "+-{:-<label_width$}-+-{:-<value_width$}-+",
        "",
        "",
        label_width = label_width,
        value_width = value_width
    );

    println!("{}", style(&border).dim());
    println!(
        "| {} | {} |",
        style(format!(
            "{:<label_width$}",
            "Metric",
            label_width = label_width
        ))
        .bold(),
        style(format!(
            "{:>value_width$}",
            "Value",
            value_width = value_width
        ))
        .bold()
    );
    println!("{}", style(&border).dim());
    for (label, value) in rows {
        println!("| {label:<label_width$} | {value:>value_width$} |");
    }
    println!("{}", style(&border).dim());
}

#[cfg(test)]
mod tests {
    use std::{
        io::Write as _,
        path::{Path, PathBuf},
    };

    use super::{
        DocLintOptions, FLAG_FAIL_ON_WARNINGS, FLAG_STATS, FLAG_SUMMARY, FilePrintMode,
        LintAggregateResult, LintRunOptions, RecursiveScanOptions, check_dir_with_print,
        check_file, check_file_with_print, discover_cddl_files, lint_dir, lint_file,
    };
    use crate::diagnostics::has_error_diagnostics;

    /// Get repo root.
    ///
    /// Should only be used in tests.
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

    fn write_temp_file(
        name: &str,
        content: &str,
    ) -> PathBuf {
        let dir = std::env::temp_dir().join("cbork_lint_test");
        std::fs::create_dir_all(&dir).expect("temp lint dir should exist");
        let path = dir.join(name);
        let mut file = std::fs::File::create(&path).expect("temp lint file should be created");
        file.write_all(content.as_bytes())
            .expect("temp lint file should be written");
        path
    }

    #[test]
    fn lint_fails_when_compiler_collects_error_diagnostics() {
        let path = write_temp_file(
            "lint_conflict_and_redundancy.cddl",
            "argon2id-any = argon2id<any>\n\
             argon2id-any = argon2id<any>\n\
             argon2id<t> = any .dtrm (tagged-argon2id<t> / untagged-argon2id<t> )\n\
             argon2id-any = bstr\n",
        );

        assert!(
            !check_file_with_print(
                &path,
                &LintRunOptions::from_flags_and_doc(0, DocLintOptions::default_off())
            ),
            "lint should fail when compiler diagnostics include an error"
        );
    }

    #[test]
    fn lint_reports_syntax_error_label_for_parse_failures() {
        let path = write_temp_file(
            "lint_syntax_error.cddl",
            "totally = invalid ) cddl syntax\n",
        );

        // Syntax errors should not show summary or stats
        assert!(
            !check_file_with_print(
                &path,
                &LintRunOptions::from_flags_and_doc(
                    FLAG_SUMMARY | FLAG_STATS,
                    DocLintOptions::default_off()
                )
            ),
            "lint should fail on syntax errors"
        );
    }

    #[test]
    fn lint_rejects_invalid_cddl_at_parser_level() {
        let path = write_temp_file("lint_parser_reject.cddl", "broken ) = cddl\n");

        // check_file itself should fail with a parse error
        let result = check_file(&path);
        assert!(result.is_err(), "check_file should fail on parse errors");
        let err = result.err().unwrap();
        let err_msg = format!("{err}");
        assert!(
            err_msg.contains("expected") || err_msg.contains("-->"),
            "error should be a syntax error, got: {err_msg}"
        );
    }

    #[test]
    fn lint_succeeds_for_library_with_unresolved_external_names() {
        let path = write_temp_file(
            "lint_library_external_refs.cddl",
            ";@ CBORK: Library\nwidget = external-value\n",
        );

        let compiled = check_file(&path).expect("library file should compile");

        assert!(compiled.is_library);
        assert!(
            !has_error_diagnostics(&compiled.warnings),
            "{:#?}",
            compiled.warnings
        );
        assert!(compiled.warnings.iter().any(|diagnostic| {
            diagnostic.code == "E016"
                && diagnostic.level == cbork_cddl_compiler::DiagnosticLevel::Warning
                && diagnostic
                    .message
                    .contains("undefined reference `external-value`")
        }));
        assert!(check_file_with_print(
            &path,
            &LintRunOptions::from_flags_and_doc(0, DocLintOptions::default_off())
        ));
    }
    #[test]
    fn lint_suppresses_declared_library_extern_warnings() {
        let path = write_temp_file(
            "lint_library_declared_extern.cddl",
            ";@ CBORK: Library\n;@ CBORK: Extern external-value\nwidget = external-value\n",
        );

        let compiled = check_file(&path).expect("library file should compile");

        assert!(compiled.is_library);
        assert!(compiled.extern_names.contains("external-value"));
        assert!(
            !compiled
                .warnings
                .iter()
                .any(|diagnostic| diagnostic.code == "E016"),
            "{:#?}",
            compiled.warnings
        );
        assert!(check_file_with_print(
            &path,
            &LintRunOptions::from_flags_and_doc(0, DocLintOptions::default_off())
        ));
    }

    #[test]
    fn strict_lint_fails_on_warnings() {
        let path = write_temp_file(
            "lint_strict_warning.cddl",
            ";@ CBORK: Library\nwidget = external-value\n",
        );

        assert!(!check_file_with_print(
            &path,
            &LintRunOptions::from_flags_and_doc(
                FLAG_FAIL_ON_WARNINGS,
                DocLintOptions::default_off()
            )
        ));
    }

    #[test]
    fn directory_lint_fails_when_any_child_file_fails() {
        let dir = std::env::temp_dir().join("cbork_lint_directory_failure");
        drop(std::fs::remove_dir_all(&dir));
        std::fs::create_dir_all(&dir).expect("temp lint dir should exist");
        std::fs::write(dir.join("valid.cddl"), "valid = uint\n")
            .expect("valid lint file should be written");
        std::fs::write(dir.join("invalid.cddl"), "broken ) = cddl\n")
            .expect("invalid lint file should be written");

        assert!(
            !check_dir_with_print(
                &dir,
                &LintRunOptions::from_flags_and_doc(0, DocLintOptions::default_off())
            ),
            "directory lint should fail when any child file fails"
        );
    }

    #[test]
    fn lint_accepts_rfc9741_legacy_ip_address_vector() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../cddl/vectors/rfc/rfc9741_legacy-ip-address.cddl");

        let compiled = check_file(&path).expect("RFC 9741 vector should compile");

        assert!(
            !has_error_diagnostics(&compiled.warnings),
            "{:#?}",
            compiled.warnings
        );
        assert!(check_file_with_print(
            &path,
            &LintRunOptions::from_flags_and_doc(0, DocLintOptions::default_off())
        ));
    }

    #[test]
    fn lint_rfc9053_test1_reports_redundant_partyinfo_only() {
        let path = repo_root().join("cddl/vectors/project/semantic-errors/rfc9053_test1.cddl");

        let compiled = check_file(&path).expect("test fixture should compile");

        assert!(
            !has_error_diagnostics(&compiled.warnings),
            "{:#?}",
            compiled.warnings
        );
        assert!(compiled.warnings.iter().any(|diagnostic| {
            diagnostic.code == "W001"
                && diagnostic
                    .message
                    .contains("redundant definition of `PartyInfo`")
        }));
    }

    #[test]
    fn lint_rfc9053_test2_reports_redundant_partyinfo_and_unreferenced_identity_val() {
        let path = repo_root().join("cddl/vectors/project/semantic-errors/rfc9053_test2.cddl");

        let compiled = check_file(&path).expect("test fixture should compile");

        assert!(compiled.warnings.iter().any(|diagnostic| {
            diagnostic.code == "W001"
                && diagnostic
                    .message
                    .contains("redundant definition of `PartyInfo`")
        }));
        assert!(compiled.warnings.iter().any(|diagnostic| {
            diagnostic.code == "E020"
                && diagnostic
                    .message
                    .contains("unreferenced top-level definition `identity-val`")
        }));
    }

    #[test]
    fn lint_rfc9053_test3_reports_two_redundancies_and_unreferenced_identity_val() {
        let path = repo_root().join("cddl/vectors/project/semantic-errors/rfc9053_test3.cddl");

        let compiled = check_file(&path).expect("test fixture should compile");

        assert!(compiled.warnings.iter().any(|diagnostic| {
            diagnostic.code == "W001"
                && diagnostic
                    .message
                    .contains("redundant definition of `PartyInfo`")
        }));
        assert!(compiled.warnings.iter().any(|diagnostic| {
            diagnostic.code == "W001"
                && diagnostic
                    .message
                    .contains("redundant definition of `identity-val`")
        }));
        assert!(compiled.warnings.iter().any(|diagnostic| {
            diagnostic.code == "E020"
                && diagnostic
                    .message
                    .contains("unreferenced top-level definition `identity-val`")
        }));
    }

    /// Helper: assert a fixture produces a single E030 whose top-level
    /// reason and/or related subdiags contain the given substring.
    /// Both `expected_reason` (in the E030 message) and
    /// `expected_in_diff` (in any related subdiag) are checked.
    fn assert_within_conflict(
        fixture: &str,
        expected_in_message: &str,
        expected_in_diff: Option<&str>,
    ) {
        let path = repo_root().join(fixture);
        let compiled = check_file(&path).expect("fixture should compile");

        let e030: Vec<_> = compiled
            .warnings
            .iter()
            .filter(|d| d.code == "E030")
            .collect();
        assert_eq!(
            e030.len(),
            1,
            "expected exactly one E030 in {fixture}, got {}: {:#?}",
            e030.len(),
            compiled.warnings
        );
        assert!(
            e030[0].message.contains(expected_in_message),
            "E030 reason mismatch in {fixture}:\n  expected substring: {expected_in_message:?}\n  got: {e030_msg}",
            e030_msg = e030[0].message
        );
        if let Some(expected_diff) = expected_in_diff {
            let found = e030[0]
                .related
                .iter()
                .any(|s| s.snippet.contains(expected_diff));
            assert!(
                found,
                "E030 related subdiags in {fixture} should contain {expected_diff:?}, got: {:#?}",
                e030[0].related
            );
        }
    }

    /// Helper: assert a fixture lints cleanly under `--strict` (no E-level
    /// diagnostics).
    fn assert_clean_lint(fixture: &str) {
        let path = repo_root().join(fixture);
        let compiled = check_file(&path).expect("fixture should compile");

        assert!(
            !has_error_diagnostics(&compiled.warnings),
            "{fixture} should lint cleanly, got: {:#?}",
            compiled.warnings
        );
    }

    // Step 8: negative `.within` / `.and` fixtures. Each must emit
    // exactly one E030 with a reason that names the failing rule.

    #[test]
    fn lint_invalid_within_cbor_dtrm_direction() {
        assert_within_conflict(
            "cddl/vectors/project/semantic-errors/invalid_within_cbor_dtrm_direction.cddl",
            ".cbor is broader than .dtrm",
            None,
        );
    }

    #[test]
    fn lint_invalid_within_missing_map_key() {
        assert_within_conflict(
            "cddl/vectors/project/semantic-errors/invalid_within_missing_map_key.cddl",
            "LHS required entry has no matching RHS entry",
            None,
        );
    }

    #[test]
    fn lint_invalid_within_required_rhs_missing() {
        assert_within_conflict(
            "cddl/vectors/project/semantic-errors/invalid_within_required_rhs_missing.cddl",
            "expected at least 1 matching entries, found 0",
            None,
        );
    }

    #[test]
    fn lint_invalid_and_empty_map() {
        assert_within_conflict(
            "cddl/vectors/project/semantic-errors/invalid_and_empty_map.cddl",
            "expected at least 1 matching entries, found 0",
            None,
        );
    }

    #[test]
    fn lint_invalid_within_choice_arm_rejected() {
        // The E030 reason must name the missing-required-RHS path and
        // drill into the rejected LHS choice arm rather than collapsing
        // the whole choice.
        assert_within_conflict(
            "cddl/vectors/project/semantic-errors/invalid_within_choice_arm_rejected.cddl",
            "nearest LHS map[0] has a compatible key but its value is rejected",
            None,
        );
    }

    // Step 8: positive `.within` / `.and` fixtures. Each must lint
    // cleanly so the fixtures act as regression guards for the
    // shape they exercise.

    #[test]
    fn lint_valid_within_dtrm_cbor() {
        assert_clean_lint("cddl/vectors/project/positive/valid_within_dtrm_cbor.cddl");
    }

    #[test]
    fn lint_valid_and_non_empty_map() {
        assert_clean_lint("cddl/vectors/project/positive/valid_and_non_empty_map.cddl");
    }

    #[test]
    fn lint_valid_within_optional_rhs_map_key() {
        assert_clean_lint("cddl/vectors/project/positive/valid_within_optional_rhs_map_key.cddl");
    }

    #[test]
    fn lint_valid_within_rfc9581_group_socket_map() {
        assert_clean_lint(
            "cddl/vectors/project/positive/valid_within_rfc9581_group_socket_map.cddl",
        );
    }

    #[test]
    fn lint_valid_generic_within_substitutes_occurrence_params() {
        assert_clean_lint(
            "cddl/vectors/project/positive/valid_generic_within_substitutes_occurrence_params.cddl",
        );
    }

    #[test]
    fn discover_cddl_files_keeps_deterministic_order() {
        let dir = std::env::temp_dir().join("cbork_lint_discovery_order");
        drop(std::fs::remove_dir_all(&dir));
        std::fs::create_dir_all(&dir).expect("temp dir should exist");
        std::fs::write(dir.join("z-last.cddl"), "z = int\n").expect("file should be written");
        std::fs::write(dir.join("a-first.cddl"), "a = int\n").expect("file should be written");
        std::fs::write(dir.join("m-middle.cddl"), "m = int\n").expect("file should be written");

        let files = discover_cddl_files(&dir, RecursiveScanOptions::default_off())
            .expect("discovery should succeed");
        let names = files
            .iter()
            .map(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_owned)
                    .expect("utf-8 filename")
            })
            .collect::<Vec<_>>();

        assert_eq!(names, vec![
            "a-first.cddl".to_owned(),
            "m-middle.cddl".to_owned(),
            "z-last.cddl".to_owned()
        ]);
    }

    // Step 2 (Optional documentation linting): marker-misuse warnings.
    //
    // The fixtures under `negative/doc_lint/` use `;!`, `;@`, or `;#` as
    // trailing comments, which the new `marker` module classifies as
    // marker misuse and reports as W030. Standalone markers in the
    // positive `doc_lint/` fixtures must not produce any W030.

    #[test]
    fn marker_misuse_warns_on_trailing_doc_marker() {
        let path = repo_root()
            .join("cddl/vectors/project/negative/doc_lint/trailing_doc_marker_misuse.cddl");
        let compiled = check_file(&path).expect("fixture should compile");

        assert!(
            compiled.warnings.iter().any(|diagnostic| {
                diagnostic.code == "W030"
                    && diagnostic
                        .message
                        .contains("special comment marker `;!` used as a trailing comment")
            }),
            "expected W030 for trailing `;!`, got: {:#?}",
            compiled.warnings
        );
    }

    #[test]
    fn marker_misuse_warns_on_trailing_cbork_directive_marker() {
        let path = repo_root()
            .join("cddl/vectors/project/negative/doc_lint/trailing_cbork_directive_misuse.cddl");
        let compiled = check_file(&path).expect("fixture should compile");

        assert!(
            compiled.warnings.iter().any(|diagnostic| {
                diagnostic.code == "W030"
                    && diagnostic
                        .message
                        .contains("special comment marker `;@` used as a trailing comment")
            }),
            "expected W030 for trailing `;@`, got: {:#?}",
            compiled.warnings
        );
    }

    #[test]
    fn marker_misuse_warns_on_trailing_include_directive_marker() {
        let path = repo_root()
            .join("cddl/vectors/project/negative/doc_lint/trailing_include_directive_misuse.cddl");
        let compiled = check_file(&path).expect("fixture should compile");

        assert!(
            compiled.warnings.iter().any(|diagnostic| {
                diagnostic.code == "W030"
                    && diagnostic
                        .message
                        .contains("special comment marker `;#` used as a trailing comment")
            }),
            "expected W030 for trailing `;#`, got: {:#?}",
            compiled.warnings
        );
    }

    #[test]
    fn marker_misuse_does_not_warn_on_standalone_doc_block() {
        let path =
            repo_root().join("cddl/vectors/project/positive/doc_lint/doc_file_with_title.cddl");
        let compiled = check_file(&path).expect("fixture should compile");

        assert!(
            !compiled.warnings.iter().any(|d| d.code == "W030"),
            "standalone `;!` markers must not trigger W030, got: {:#?}",
            compiled.warnings
        );
    }

    #[test]
    fn marker_misuse_does_not_warn_on_standalone_cbork_library() {
        let path = repo_root()
            .join("cddl/vectors/project/positive/doc_lint/doc_exported_definition_with_h3.cddl");
        let compiled = check_file(&path).expect("fixture should compile");

        assert!(
            !compiled.warnings.iter().any(|d| d.code == "W030"),
            "standalone `;@` markers must not trigger W030, got: {:#?}",
            compiled.warnings
        );
    }

    #[test]
    fn marker_misuse_does_not_warn_on_standalone_include_directive() {
        // Use a self-contained file with no `;#` directives and a
        // separate standalone `#`-comment to confirm standalone markers
        // do not trigger W030. The actual include-directive path is
        // covered by the unit tests in `cbork-cddl-compiler::marker`,
        // which can call the classifier without needing a real include
        // file on disk.
        let dir = std::env::temp_dir().join("cbork_marker_misuse_test");
        std::fs::create_dir_all(&dir).expect("temp dir should exist");
        let path = dir.join("standalone_include.cddl");
        std::fs::write(&path, "root = 1\n").expect("fixture should be written");

        let compiled = check_file(&path).expect("fixture should compile");
        assert!(
            !compiled.warnings.iter().any(|d| d.code == "W030"),
            "an ordinary CDDL file with no special markers must not trigger W030, got: {:#?}",
            compiled.warnings
        );
    }

    // Step 3: trailing directive markers must warn AND must not apply.
    //
    // The negative fixtures under `negative/doc_lint/` now use the
    // canonical directive forms (`;@ CBORK: Library`, `;# include "..."`).
    // Before step 3 the trailing forms would either set the file as a
    // library (silently applying the directive) or trigger an include
    // resolution error. After step 3 the directive is skipped, the file
    // is treated as an ordinary schema, and the include is never
    // attempted. Only the W030 marker-misuse warning should remain.

    #[test]
    fn trailing_cbork_library_marker_does_not_apply() {
        let path = repo_root()
            .join("cddl/vectors/project/negative/doc_lint/trailing_cbork_directive_misuse.cddl");
        let compiled = check_file(&path).expect("fixture should compile");

        assert!(
            !compiled.is_library,
            "a trailing `;@ CBORK: Library` must not mark the file as a library, \
             but is_library was true. Diagnostics: {:#?}",
            compiled.warnings
        );
        assert!(
            !compiled
                .warnings
                .iter()
                .any(|d| d.code == "E018" || d.code == "E019"),
            "trailing `;@ CBORK: Library` must not produce E018/E019, got: {:#?}",
            compiled.warnings
        );
    }

    #[test]
    fn trailing_include_directive_marker_does_not_apply() {
        let path = repo_root()
            .join("cddl/vectors/project/negative/doc_lint/trailing_include_directive_misuse.cddl");
        let compiled = check_file(&path).expect(
            "trailing `;# include` must not attempt resolution; \
             the include path is intentionally absent so any attempt to \
             resolve it would surface as an E009/E011",
        );

        assert!(
            !compiled
                .warnings
                .iter()
                .any(|d| d.code == "E009" || d.code == "E011"),
            "trailing `;# include` must not trigger include resolution diagnostics, \
             got: {:#?}",
            compiled.warnings
        );
    }

    #[test]
    fn trailing_cbork_library_marker_still_warns() {
        let path = repo_root()
            .join("cddl/vectors/project/negative/doc_lint/trailing_cbork_directive_misuse.cddl");
        let compiled = check_file(&path).expect("fixture should compile");

        assert!(
            compiled
                .warnings
                .iter()
                .any(|d| { d.code == "W030" && d.message.contains("special comment marker `;@`") }),
            "trailing `;@` must still emit W030, got: {:#?}",
            compiled.warnings
        );
    }

    #[test]
    fn trailing_include_directive_marker_still_warns() {
        let path = repo_root()
            .join("cddl/vectors/project/negative/doc_lint/trailing_include_directive_misuse.cddl");
        let compiled = check_file(&path).expect("fixture should compile");

        assert!(
            compiled
                .warnings
                .iter()
                .any(|d| { d.code == "W030" && d.message.contains("special comment marker `;#`") }),
            "trailing `;#` must still emit W030, got: {:#?}",
            compiled.warnings
        );
    }

    // Step 2 (Optional documentation linting, continued): the W036
    // doc-marker-spacing rule.
    //
    // `;!Something` (no space after the marker) is treated as a soft
    // warning under `--doc`, not under normal lint, so the canonical
    // shape `;! ` is the only one that does not produce a diagnostic.
    // `;!` on its own line, or `;!` followed only by whitespace, is
    // valid because those lines represent blank lines inside a doc
    // block. Trailing `;!X` comments are owned by W030, not W036.

    #[test]
    fn doc_marker_spacing_does_not_warn_under_normal_lint() {
        let path = write_temp_file(
            "w036_off.cddl",
            ";! # Title\n;!WithSpaceBug\n;! OkLine\nrule = 1\n",
        );
        let compiled = check_file(&path).expect("fixture should compile");

        assert!(
            !compiled.warnings.iter().any(|d| d.code == "W036"),
            "W036 must not fire during normal lint, got: {:#?}",
            compiled.warnings
        );
        let _unused = std::fs::remove_file(&path);
    }

    #[test]
    fn doc_marker_spacing_warns_under_doc_lint() {
        let path = write_temp_file(
            "w036_on.cddl",
            ";! # Title\n;!WithSpaceBug\n;! OkLine\nrule = 1\n",
        );
        let source_text = std::fs::read_to_string(&path).expect("read fixture");

        // Build the warning set the way `run_doc_lint` does and
        // exercise the new helper directly to keep the test focused.
        let diagnostics = cbork_cddl_compiler::collect_marker_spacing_issues(
            &check_file(&path).expect("compile").user_nodes,
            &source_text,
        );

        assert!(
            diagnostics.iter().any(|d| d.code == "W036"),
            "W036 must fire under --doc, got: {diagnostics:?}"
        );
        let _unused = std::fs::remove_file(&path);
    }

    #[test]
    fn doc_marker_spacing_skips_canonical_and_blank_doc_lines() {
        // A canonical `;!` line and a `;!`-only line must not warn;
        // only the offending `;!X` line triggers W036.
        let path = write_temp_file(
            "w036_mixed.cddl",
            ";! # Title\n;!\n;!BadLine\n;! GoodLine\nrule = 1\n",
        );
        let compiled = check_file(&path).expect("compile fixture");
        let source_text = std::fs::read_to_string(&path).expect("read fixture");
        let diagnostics =
            cbork_cddl_compiler::collect_marker_spacing_issues(&compiled.user_nodes, &source_text);
        let w036: Vec<_> = diagnostics.iter().filter(|d| d.code == "W036").collect();
        let _unused = std::fs::remove_file(&path);
        assert_eq!(
            w036.len(),
            1,
            "exactly one W036 expected for the offending line, got: {diagnostics:?}"
        );
        assert!(
            w036[0].message.contains("`;!B`"),
            "diagnostic must point at the offending line, got: {}",
            w036[0].message
        );
    }

    // Step 11 (continued): the --fix post-process trim pass.
    //
    // The reverse transform is the consumer of every `--doc --fix`
    // write-back; the trailing-whitespace trim pass is added there.
    // These integration tests exercise the public API end-to-end
    // through `transform_to_markdown` + `reverse_transform`, mirroring
    // how `run_doc_lint` drives them.

    #[test]
    fn fix_post_process_strips_trailing_whitespace() {
        let source = ";! # Title   \n\nrule = 1  \n\t\nrule_b = 2\t\n";
        let synthetic = cbork_cddl_compiler::transform_to_markdown(source);
        let reconstructed =
            cbork_cddl_compiler::reverse_transform(&synthetic.text, source, &synthetic)
                .expect("reverse transform must succeed");
        assert!(
            !reconstructed.contains("   \n"),
            "trailing spaces must be stripped, got:\n{reconstructed}"
        );
        assert!(
            !reconstructed.contains("\t\n"),
            "trailing tabs must be stripped, got:\n{reconstructed}"
        );
        assert!(
            reconstructed.contains("rule = 1\n"),
            "CDDL rule must survive, got:\n{reconstructed}"
        );
    }

    #[test]
    fn fix_post_process_replaces_whitespace_only_lines_with_empty() {
        let source = ";! # Title\n  \t \nrule = 1\n";
        let synthetic = cbork_cddl_compiler::transform_to_markdown(source);
        let reconstructed =
            cbork_cddl_compiler::reverse_transform(&synthetic.text, source, &synthetic)
                .expect("reverse transform must succeed");
        assert!(
            !reconstructed.contains("  \t \n"),
            "whitespace-only line must be reduced, got:\n{reconstructed}"
        );
        // The blank line between `;! # Title` and `rule = 1` must
        // still exist as a blank line; the trim only erases the
        // whitespace on it.
        assert!(
            reconstructed.contains(";! # Title\n\nrule = 1\n"),
            "blank gap between doc and rule must be preserved, got:\n{reconstructed}"
        );
    }

    // Step 4: documentation block scanner.
    //
    // The scanner runs on the captured pre-transform CDDL text and
    // groups standalone `;!` lines into blocks while tracking the
    // binding model from `crates/cbork/plan.md` § *Documentation
    // binding model*. These integration tests load each fixture,
    // call `scan_doc_blocks` on the file text, and assert the
    // resulting blocks and bindings match the fixture's intent.

    fn read_fixture(rel: &str) -> String {
        let path = repo_root().join(rel);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {rel}: {e}"))
    }

    #[test]
    fn scanner_groups_file_title_block_in_positive_fixture() {
        let source =
            read_fixture("cddl/vectors/project/positive/doc_lint/doc_file_with_title.cddl");
        let scan = cbork_cddl_compiler::scan_doc_blocks(&source);

        assert_eq!(scan.blocks.len(), 1, "expected one doc block");
        let binding = &scan.bindings[0];
        assert!(
            binding.is_file_level,
            "the lone doc block must be the file-level doc, got binding {binding:?}"
        );
        // The fixture intentionally inserts a blank line plus regular
        // comments between the doc block and the rule, so the block is
        // not expected to bind to the rule. The file-level doc stands
        // on its own; the rule's documentation lives elsewhere.
        assert_eq!(
            binding.definition_line, None,
            "the blank line between the doc block and the rule must break the binding"
        );
    }

    #[test]
    fn scanner_keeps_two_blocks_separate_in_two_blocks_fixture() {
        let source =
            read_fixture("cddl/vectors/project/positive/doc_lint/doc_fix_two_blocks_input.cddl");
        let scan = cbork_cddl_compiler::scan_doc_blocks(&source);

        assert_eq!(scan.blocks.len(), 3, "expected three doc blocks");
        assert!(scan.bindings[0].is_file_level);

        // Doc block 1 ends at the first blank line, so it is the
        // file-level doc and is not bound to any definition.
        assert_eq!(scan.bindings[0].definition_line, None);

        // Doc blocks 2 and 3 are each followed by a blank line before
        // their target rule, so they do not bind. The fixture exists to
        // prove that the scanner keeps them as separate blocks even
        // when separated by nothing but blank lines.
        assert!(!scan.bindings[1].is_file_level);
        assert_eq!(scan.bindings[1].definition_line, None);

        assert!(!scan.bindings[2].is_file_level);
        assert_eq!(scan.bindings[2].definition_line, None);
    }

    #[test]
    fn scanner_preserves_blank_lines_in_blank_lines_fixture() {
        let source =
            read_fixture("cddl/vectors/project/positive/doc_lint/doc_fix_blank_lines_input.cddl");
        let scan = cbork_cddl_compiler::scan_doc_blocks(&source);

        assert_eq!(
            scan.blocks.len(),
            2,
            "expected two doc blocks: the root docs and the main fixture docs"
        );
        assert!(scan.bindings[0].is_file_level);
        assert!(!scan.bindings[1].is_file_level);
        // The fixture inserts multiple consecutive blank lines between
        // the doc block and the rules, so neither block binds.
        assert_eq!(scan.bindings[0].definition_line, None);
        assert_eq!(scan.bindings[1].definition_line, None);
    }

    #[test]
    fn scanner_handles_exported_definition_with_h3_fixture() {
        let source = read_fixture(
            "cddl/vectors/project/positive/doc_lint/doc_exported_definition_with_h3.cddl",
        );
        let scan = cbork_cddl_compiler::scan_doc_blocks(&source);

        assert_eq!(scan.blocks.len(), 2, "file docs + definition docs");
        assert!(scan.bindings[0].is_file_level);
        assert_eq!(
            scan.bindings[0].definition_line, None,
            "a blank line separates the file docs from the first definition"
        );

        assert!(!scan.bindings[1].is_file_level);
        assert!(
            scan.bindings[1].definition_line.is_some(),
            "the definition doc must bind to its target rule"
        );
    }

    #[test]
    fn scanner_strips_only_bang_marker_and_preserves_markdown_text() {
        let source = "; source: project\n;! # Title\n;! Description\nrule = 1\n";
        let scan = cbork_cddl_compiler::scan_doc_blocks(source);

        assert_eq!(scan.blocks.len(), 1);
        let block = &scan.blocks[0];
        assert_eq!(block.lines.len(), 2);
        assert_eq!(block.lines[0].text, " # Title");
        assert_eq!(block.lines[1].text, " Description");
        // The first non-blank, non-doc `;` line is `; source: project`.
        // That is `OtherComment`, not `DocLine`, so it is not part of
        // the block.
        assert_eq!(block.start_line, 2);
        assert_eq!(block.end_line, 3);
    }

    // Step 5: CDDL-to-Markdown transform.
    //
    // The transform strips standalone `;!` markers, replaces every
    // non-doc CDDL span with a `<!-- CBORK CDDL FROM start-end -->`
    // splice marker, and wraps each splice marker in a blank line on
    // each side. The tests below exercise the public
    // `cbork_cddl_compiler::transform_to_markdown` API against the
    // doc-lint fixtures.

    #[test]
    fn transform_emits_splice_marker_for_fixture_with_one_doc_block() {
        let source =
            read_fixture("cddl/vectors/project/positive/doc_lint/doc_file_with_title.cddl");
        let synthetic = cbork_cddl_compiler::transform_to_markdown(&source);

        // The fixture has one file-level doc block; the synthetic
        // output should contain at least one DocLine and at least one
        // SpliceMarker for the surrounding CDDL.
        let doc_count = synthetic
            .lines
            .iter()
            .filter(|l| {
                matches!(
                    l.kind,
                    cbork_cddl_compiler::SyntheticLineKind::DocLine { .. }
                )
            })
            .count();
        let splice_count = synthetic
            .lines
            .iter()
            .filter(|l| {
                matches!(
                    l.kind,
                    cbork_cddl_compiler::SyntheticLineKind::SpliceMarker { .. }
                )
            })
            .count();

        assert!(
            doc_count >= 1,
            "expected at least one doc line, got {synthetic:#?}"
        );
        assert!(
            splice_count >= 1,
            "expected at least one splice marker, got {synthetic:#?}"
        );

        // The fixture's `doc_fixture_file_with_title = 1` rule must be
        // covered by some splice marker.
        let covered = synthetic.lines.iter().any(|l| {
            match &l.kind {
                cbork_cddl_compiler::SyntheticLineKind::SpliceMarker {
                    span_start,
                    span_end,
                } => *span_start <= 16 && *span_end >= 16,
                _ => false,
            }
        });
        assert!(
            covered,
            "the fixture's rule on line 16 must be inside a splice span"
        );
    }

    #[test]
    fn transform_produces_separate_splice_markers_around_blank_line_in_two_blocks_fixture() {
        let source =
            read_fixture("cddl/vectors/project/positive/doc_lint/doc_fix_two_blocks_input.cddl");
        let synthetic = cbork_cddl_compiler::transform_to_markdown(&source);

        // The fixture has three doc blocks (root, first, second) plus
        // a leading non-doc source-comment. The blank lines between
        // them must be folded into splice markers so the Markdown
        // engine cannot merge the blocks.
        let splice_spans: Vec<_> = synthetic
            .lines
            .iter()
            .filter_map(|l| cbork_cddl_compiler::splice_span(&l.kind))
            .collect();

        assert_eq!(
            splice_spans.len(),
            4,
            "expected four splice markers (one before the file-level block plus three between/after definition blocks), got {splice_spans:?}"
        );

        // Adjacent splice markers must not overlap. If they did, the
        // blank-line span would have been split into two markers that
        // cover the same source line.
        for pair in splice_spans.windows(2) {
            let (prev_start, prev_end) = pair[0];
            let (next_start, _next_end) = pair[1];
            assert!(
                prev_end < next_start,
                "splice markers must not overlap or touch: {prev_start}-{prev_end} and {next_start}-..."
            );
        }
    }

    #[test]
    fn transform_strips_bang_marker_and_separator_in_doc_lines() {
        let source = ";! # Title\nrule = 1\n";
        let synthetic = cbork_cddl_compiler::transform_to_markdown(source);

        let doc_line = synthetic
            .lines
            .iter()
            .find_map(|l| {
                match &l.kind {
                    cbork_cddl_compiler::SyntheticLineKind::DocLine { source_line, .. } => {
                        Some((*source_line, l.text.clone()))
                    },
                    _ => None,
                }
            })
            .expect("expected at least one doc line");
        assert_eq!(
            doc_line.0, 1,
            "doc line must report its 1-based source line"
        );
        assert_eq!(
            doc_line.1, "# Title",
            "doc line text must have the `;!` marker and common indent stripped"
        );
        assert!(
            !synthetic.text.contains(";!"),
            "transform output must not contain any `;!` markers, got:\n{}",
            synthetic.text
        );
    }

    #[test]
    fn transform_inlines_only_doc_comments_no_splice_markers() {
        // When the source contains nothing but standalone `;!` lines,
        // the synthetic output is exactly the stripped and dedented Markdown text
        // with no splice markers and no blank lines.
        let source = ";! # Title\n;! Description\n";
        let synthetic = cbork_cddl_compiler::transform_to_markdown(source);

        assert_eq!(synthetic.lines.len(), 2);
        assert!(synthetic.lines.iter().all(|l| {
            matches!(
                l.kind,
                cbork_cddl_compiler::SyntheticLineKind::DocLine { .. }
            )
        }));
        assert_eq!(synthetic.text, "# Title\nDescription");
    }

    // Step 9 / Step 10 CLI integration: the doc-lint fixtures under
    // `cddl/vectors/project/{positive,negative}/doc_lint/` exercise the
    // real `;@ CBORK: Export` directive model. The fixture-local
    // `cddl/vectors/project/.rumdl.toml` disables the noisy style rules
    // so the only diagnostics come from the semantic checks.

    fn run_doc_lint_cli(rel: &str) -> (i32, String) {
        let path = repo_root().join(rel);
        let output = std::process::Command::new(env!("CARGO"))
            .arg("run")
            .arg("--quiet")
            .arg("--bin")
            .arg("cbork")
            .arg("lint")
            .arg("--doc")
            .arg(&path)
            .arg("--no-banner")
            .output()
            .expect("run cbork lint --doc");
        let exit_code = output.status.code().unwrap_or(-1);
        let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
        combined.push_str(&String::from_utf8_lossy(&output.stderr));
        (exit_code, combined)
    }

    #[test]
    fn cli_doc_lint_positive_exported_definition_with_h3_passes() {
        let (code, output) = run_doc_lint_cli(
            "cddl/vectors/project/positive/doc_lint/doc_exported_definition_with_h3.cddl",
        );
        assert_eq!(
            code, 0,
            "expected positive fixture to pass, got exit {code} and output:\n{output}"
        );
        assert!(
            !output.contains("E030")
                && !output.contains("E031")
                && !output.contains("E032")
                && !output.contains("E033"),
            "positive fixture must not emit doc-semantic diagnostics, got:\n{output}"
        );
    }

    #[test]
    fn cli_doc_lint_negative_exported_missing_docs_emits_e032() {
        let (code, output) = run_doc_lint_cli(
            "cddl/vectors/project/negative/doc_lint/doc_exported_missing_docs.cddl",
        );
        assert_eq!(
            code, 0,
            "expected negative fixture to fail with warnings but exit 0 (no errors), got {code}:\n{output}"
        );
        assert!(
            output.contains("E032"),
            "negative fixture must emit E032 for the missing exported docs, got:\n{output}"
        );
        assert!(
            output.contains("`location-references`"),
            "E032 must name the exported rule, got:\n{output}"
        );
    }

    #[test]
    fn cli_doc_lint_negative_exported_generic_missing_param_emits_e033() {
        let (code, output) = run_doc_lint_cli(
            "cddl/vectors/project/negative/doc_lint/doc_exported_generic_missing_param.cddl",
        );
        assert_eq!(
            code, 0,
            "expected exit 0 (no errors), got {code}:\n{output}"
        );
        assert!(
            output.contains("E033"),
            "negative fixture must emit E033 for missing generic param, got:\n{output}"
        );
        assert!(
            output.contains("`key`"),
            "E033 must name the missing parameter, got:\n{output}"
        );
    }

    #[test]
    fn cli_doc_lint_negative_file_missing_h1_emits_e030() {
        let (code, output) =
            run_doc_lint_cli("cddl/vectors/project/negative/doc_lint/doc_file_missing_h1.cddl");
        assert_eq!(code, 0, "expected exit 0, got {code}:\n{output}");
        assert!(
            output.contains("E030"),
            "negative fixture must emit E030 for missing h1, got:\n{output}"
        );
    }

    #[test]
    fn cli_doc_lint_negative_definition_h2_emits_e031() {
        let (code, output) = run_doc_lint_cli(
            "cddl/vectors/project/negative/doc_lint/doc_definition_h2_heading.cddl",
        );
        assert_eq!(code, 0, "expected exit 0, got {code}:\n{output}");
        assert!(
            output.contains("E031"),
            "negative fixture must emit E031 for h2-in-definition, got:\n{output}"
        );
    }

    // Steps 6, 7, 8: doc-source safety validation, `rumdl` integration,
    // and diagnostic mapping. The tests below exercise the public API
    // surface of `cbork_cddl_compiler::doc_lint`.

    // Step 11: --doc --fix end-to-end with a fixture-local rumdl config
    // that enables MD022 (blanks-around-headings). The previous step 10
    // tests only exercise the project-level .rumdl.toml which disables
    // all style rules; this test proves the reverse transform works
    // when rumdl actually performs heading-blank and MD023 fixes.

    #[test]
    fn cli_doc_lint_fix_writes_modified_cddl_to_disk() {
        let dir = std::env::temp_dir().join("cbork_doc_fix_cli");
        let _unused = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        std::fs::write(
            dir.join(".rumdl.toml"),
            "[global]\ndisable = [\"MD001\", \"MD063\", \"MD013\", \"MD041\"]\n",
        )
        .expect("write rumdl config");
        let fixture = dir.join("fix_test.cddl");
        std::fs::write(
            &fixture,
            concat!(
                "\
;! # Title
;!
;! Description
",
                "rule = 1   \t\n",
            ),
        )
        .expect("write fixture");

        let output = std::process::Command::new(env!("CARGO"))
            .arg("run")
            .arg("--quiet")
            .arg("--bin")
            .arg("cbork")
            .arg("lint")
            .arg("--doc")
            .arg("--fix")
            .arg(&fixture)
            .arg("--no-banner")
            .output()
            .expect("run cbork lint --doc --fix");
        let combined = String::from_utf8_lossy(&output.stdout).into_owned()
            + &String::from_utf8_lossy(&output.stderr);

        let _unused = std::fs::remove_dir_all(&dir);

        assert!(
            combined.contains("W035"),
            "--doc --fix must report W035 (written to disk), got:\n{combined}"
        );
        assert!(
            !combined.contains("E035"),
            "--doc --fix must not reject the fix with E035, got:\n{combined}"
        );
    }

    #[test]
    fn cli_doc_lint_fix_does_not_write_when_content_is_unchanged() {
        let dir = std::env::temp_dir().join(format!("cbork_doc_fix_noop_{}", std::process::id()));
        let _unused = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        std::fs::write(dir.join(".rumdl.toml"), "[global]\ndisable = [\"all\"]\n")
            .expect("write rumdl config");
        let fixture = dir.join("noop.cddl");
        let source = "\
;! # Noop Fixture
;! Already formatted documentation.
rule = 1
";
        std::fs::write(&fixture, source).expect("write fixture");
        let before_modified = std::fs::metadata(&fixture)
            .expect("fixture metadata before")
            .modified()
            .expect("fixture mtime before");

        std::thread::sleep(std::time::Duration::from_millis(1100));

        let output = std::process::Command::new(env!("CARGO"))
            .arg("run")
            .arg("--quiet")
            .arg("--bin")
            .arg("cbork")
            .arg("lint")
            .arg("--doc")
            .arg("--fix")
            .arg(&fixture)
            .arg("--no-banner")
            .output()
            .expect("run cbork lint --doc --fix");
        let combined = String::from_utf8_lossy(&output.stdout).into_owned()
            + &String::from_utf8_lossy(&output.stderr);
        let after_modified = std::fs::metadata(&fixture)
            .expect("fixture metadata after")
            .modified()
            .expect("fixture mtime after");
        let after = std::fs::read_to_string(&fixture).expect("read fixture after");
        let _unused = std::fs::remove_dir_all(&dir);

        assert!(
            output.status.success(),
            "cbork lint --doc --fix must succeed, got:\n{combined}"
        );
        assert!(
            !combined.contains("W035"),
            "no-op --doc --fix must not report a write, got:\n{combined}"
        );
        assert_eq!(after, source, "no-op --doc --fix must not change content");
        assert_eq!(
            after_modified, before_modified,
            "no-op --doc --fix must not update file mtime"
        );
    }

    // Step 12: remaining CLI integration tests.
    //
    // Tests below exercise the fixtures that exercise positive paths
    // (fenced code blocks, valid file title), negative paths
    // (trailing marker misuse W030), diagnostic position rendering,
    // and `--doc --fix` CDDL source preservation.

    #[test]
    fn cli_doc_lint_positive_file_with_title_passes() {
        let (code, output) =
            run_doc_lint_cli("cddl/vectors/project/positive/doc_lint/doc_file_with_title.cddl");
        assert_eq!(code, 0, "expected exit 0, got {code}:\n{output}");
        assert!(
            !output.contains("E030"),
            "file-with-title fixture must not emit E030, got:\n{output}"
        );
    }

    #[test]
    fn cli_doc_lint_positive_fenced_cddl_block_passes() {
        let (code, output) = run_doc_lint_cli(
            "cddl/vectors/project/positive/doc_lint/doc_with_fenced_cddl_block.cddl",
        );
        assert_eq!(code, 0, "expected exit 0, got {code}:\n{output}");
    }

    #[test]
    fn cli_doc_lint_trailing_doc_marker_emits_w030() {
        let (code, output) = run_doc_lint_cli(
            "cddl/vectors/project/negative/doc_lint/trailing_doc_marker_misuse.cddl",
        );
        assert_eq!(code, 0, "expected exit 0, got {code}:\n{output}");
        assert!(
            output.contains("W030"),
            "trailing `;!` marker must emit W030, got:\n{output}"
        );
    }

    #[test]
    fn cli_doc_lint_trailing_cbork_directive_marker_emits_w030() {
        let (code, output) = run_doc_lint_cli(
            "cddl/vectors/project/negative/doc_lint/trailing_cbork_directive_misuse.cddl",
        );
        assert_eq!(code, 0, "expected exit 0, got {code}:\n{output}");
        assert!(
            output.contains("W030"),
            "trailing `;@` marker must emit W030, got:\n{output}"
        );
    }

    #[test]
    fn cli_doc_lint_trailing_include_marker_emits_w030() {
        let (code, output) = run_doc_lint_cli(
            "cddl/vectors/project/negative/doc_lint/trailing_include_directive_misuse.cddl",
        );
        assert_eq!(code, 0, "expected exit 0, got {code}:\n{output}");
        assert!(
            output.contains("W030"),
            "trailing `;#` marker must emit W030, got:\n{output}"
        );
    }

    // Step 12: fix-preservation tests. These verify that `--doc --fix`
    // preserves blank-line gaps between doc blocks and does not alter
    // non-doc CDDL source.

    #[test]
    fn cli_doc_lint_fix_preserves_blank_line_gaps_between_blocks() {
        let dir = std::env::temp_dir().join("cbork_doc_fix_gaps");
        let _unused = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        std::fs::write(
            dir.join(".rumdl.toml"),
            "[global]\ndisable = [\"MD001\", \"MD063\", \"MD013\", \"MD041\"]\n",
        )
        .expect("write rumdl config");
        let fixture = dir.join("gaps.cddl");
        std::fs::write(
            &fixture,
            "\
;! # First block

;! # Second block
rule_a = 1

;! # Third block
rule_b = 2
",
        )
        .expect("write fixture");

        let output = std::process::Command::new(env!("CARGO"))
            .arg("run")
            .arg("--quiet")
            .arg("--bin")
            .arg("cbork")
            .arg("lint")
            .arg("--doc")
            .arg("--fix")
            .arg(&fixture)
            .arg("--no-banner")
            .output()
            .expect("run cbork lint --doc --fix");
        let combined = String::from_utf8_lossy(&output.stdout).into_owned()
            + &String::from_utf8_lossy(&output.stderr);
        // Must read the modified file BEFORE removing the temp dir.
        let modified = std::fs::read_to_string(&fixture).expect("read modified fixture");
        let _unused = std::fs::remove_dir_all(&dir);

        assert!(combined.contains("W035"), "expected W035, got:\n{combined}");
        // The CDDL rules themselves must not have changed.
        assert!(
            modified.contains("rule_a = 1"),
            "rule_a must be preserved:\n{modified}"
        );
        assert!(
            modified.contains("rule_b = 2"),
            "rule_b must be preserved:\n{modified}"
        );
    }

    #[test]
    fn cli_doc_lint_fix_preserves_blank_doc_lines_within_block() {
        // Feature 004 regression guard: blank `;!` doc lines inside a
        // continuous doc block must survive `cbork lint --doc --fix`.
        // Previously the reverse transform stripped them.
        let dir = std::env::temp_dir().join("cbork_doc_fix_blank_doc_lines");
        let _unused = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        std::fs::write(
            dir.join(".rumdl.toml"),
            "[global]\ndisable = [\"MD001\", \"MD063\", \"MD013\", \"MD041\"]\n",
        )
        .expect("write rumdl config");
        let fixture = dir.join("blank_doc.cddl");
        std::fs::write(
            &fixture,
            concat!(
                "\
;! # Name Registration Payload v1
;!
;! ## Overview
;!
;! This file defines the v1 name-registration payload that is carried inside a blockchain transaction.
;! The blockchain transaction wrapper itself is out of scope here.

",
                "rule = 1   \t\n",
            ),
        )
        .expect("write fixture");

        let output = std::process::Command::new(env!("CARGO"))
            .arg("run")
            .arg("--quiet")
            .arg("--bin")
            .arg("cbork")
            .arg("lint")
            .arg("--doc")
            .arg("--fix")
            .arg(&fixture)
            .arg("--no-banner")
            .output()
            .expect("run cbork lint --doc --fix");
        let combined = String::from_utf8_lossy(&output.stdout).into_owned()
            + &String::from_utf8_lossy(&output.stderr);
        // Must read the modified file BEFORE removing the temp dir.
        let modified = std::fs::read_to_string(&fixture).expect("read modified fixture");
        let _unused = std::fs::remove_dir_all(&dir);

        assert!(
            combined.contains("W035"),
            "--doc --fix must report W035 (wrote fixed CDDL to disk), got:\n{combined}"
        );
        assert!(
            !combined.contains("E035"),
            "--doc --fix must not reject the fix with E035, got:\n{combined}"
        );
        // The blank `;!` lines between the headings and the body must
        // survive.
        assert!(
            modified.contains(
                ";! # Name Registration Payload v1\n;!\n;! ## Overview\n;!\n;! This file defines"
            ),
            "blank `;!` lines between heading and body must survive --fix, got:\n{modified}"
        );
        // The CDDL rules themselves must not have changed.
        assert!(
            modified.contains("rule = 1"),
            "rule = 1 must be preserved:\n{modified}"
        );
        // Triple blank lines must not appear: collapse_blank_doc_lines
        // collapses runs of `;!` to a single `;!`.
        assert!(
            !modified.contains(";!\n;!\n;!\n"),
            "must not contain three or more consecutive blank doc lines:\n{modified}"
        );
    }

    #[test]
    fn cli_doc_lint_fix_is_idempotent_for_nested_lists() {
        let dir = std::env::temp_dir().join("cbork_doc_fix_idempotent_nested");
        let _unused = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        std::fs::write(
            dir.join(".rumdl.toml"),
            "[global]\ndisable = [\"MD001\", \"MD063\", \"MD013\", \"MD041\"]\n",
        )
        .expect("write rumdl config");
        let fixture = dir.join("nested.cddl");
        std::fs::write(
            &fixture,
            "\
;! 3. For each recipient:
;!    a. Derive the blinded recipient tag from the context hash and
;!       recipient public key.
;!    b. HPKE-seal the CEK to the recipient's public key using the
;!       X-Wing KEM.
;!    c. Store the 1120-byte encapsulation in unprotected header `-4`.
;!    d. Store the 48-byte encrypted CEK as the recipient ciphertext.
;!    e. Build HPKE-9-KE protected headers: `{1: 57, 4: tag}`.
;! 4. Emit `[headers, ciphertext, [recipient1, ...]]`.

rule = 1
",
        )
        .expect("write fixture");

        let run_fix = || {
            let output = std::process::Command::new(env!("CARGO"))
                .arg("run")
                .arg("--quiet")
                .arg("--bin")
                .arg("cbork")
                .arg("lint")
                .arg("--doc")
                .arg("--fix")
                .arg(&fixture)
                .arg("--no-banner")
                .output()
                .expect("run cbork lint --doc --fix");
            let combined = String::from_utf8_lossy(&output.stdout).into_owned()
                + &String::from_utf8_lossy(&output.stderr);
            assert!(
                output.status.success(),
                "cbork lint --doc --fix must succeed, got:\n{combined}"
            );
            combined
        };

        let first_output = run_fix();
        let after_first = std::fs::read_to_string(&fixture).expect("read after first fix");
        let second_output = run_fix();
        let after_second = std::fs::read_to_string(&fixture).expect("read after second fix");
        let third_output = run_fix();
        let after_third = std::fs::read_to_string(&fixture).expect("read after third fix");
        let _unused = std::fs::remove_dir_all(&dir);

        for output in [&first_output, &second_output, &third_output] {
            assert!(
                !output.contains("MD077"),
                "`--doc --fix` output must describe post-fix diagnostics, not fixed pre-fix MD077 warnings:\n{output}"
            );
        }

        assert_eq!(
            after_first, after_second,
            "`--doc --fix` must be stable on the second run.\n\
             after first:\n{after_first}\n---\nafter second:\n{after_second}"
        );
        assert_eq!(
            after_second, after_third,
            "`--doc --fix` must stay stable on later runs.\n\
             after second:\n{after_second}\n---\nafter third:\n{after_third}"
        );
        assert!(
            !after_first.contains("For each recipient: a."),
            "sub-list marker `a.` must not be joined onto the parent line:\n{after_first}"
        );
        assert!(
            !after_first.contains("recipient public key. b."),
            "sub-list marker `b.` must not be joined onto its paragraph:\n{after_first}"
        );
        assert!(
            after_first.contains(";!    recipient public key."),
            "fixed continuation indentation must be preserved on rerun:\n{after_first}"
        );
        assert!(
            !after_first.contains("X-Wing KEM. c."),
            "sub-list marker `c.` must not be joined onto its paragraph:\n{after_first}"
        );
        assert!(
            after_first.contains(";!    X-Wing KEM."),
            "fixed continuation indentation must be preserved on rerun:\n{after_first}"
        );
        assert!(
            !after_first.contains("header `-4`. d."),
            "sub-list marker `d.` must not be joined onto its paragraph:\n{after_first}"
        );
        assert!(
            !after_first.contains("recipient ciphertext. e."),
            "sub-list marker `e.` must not be joined onto its paragraph:\n{after_first}"
        );
    }

    #[test]
    fn cli_doc_lint_fix_rechecks_exported_docs_with_fresh_line_numbers() {
        let dir =
            std::env::temp_dir().join(format!("cbork_doc_fix_fresh_lines_{}", std::process::id()));
        let _unused = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        std::fs::write(
            dir.join(".rumdl.toml"),
            "[global]\ndisable = [\"MD001\", \"MD063\", \"MD013\", \"MD041\"]\n",
        )
        .expect("write rumdl config");
        let fixture = dir.join("fresh_lines.cddl");
        std::fs::write(
            &fixture,
            "\
;! # Fixture
;! File docs.
;@ CBORK: Library

;! ### COSE_Encrypt0
;! Documented exported generic definition.
;!
;! **`headers`** are the protected headers.
;! **`payload`** is the plaintext model.
;@ CBORK: Export
COSE_Encrypt0<headers, payload> = [
  headers,
  payload: bstr,
]
",
        )
        .expect("write fixture");

        let output = std::process::Command::new(env!("CARGO"))
            .arg("run")
            .arg("--quiet")
            .arg("--bin")
            .arg("cbork")
            .arg("lint")
            .arg("--doc")
            .arg("--fix")
            .arg(&fixture)
            .arg("--no-banner")
            .output()
            .expect("run cbork lint --doc --fix");
        let combined = String::from_utf8_lossy(&output.stdout).into_owned()
            + &String::from_utf8_lossy(&output.stderr);
        let _unused = std::fs::remove_dir_all(&dir);

        assert!(
            output.status.success(),
            "cbork lint --doc --fix must succeed, got:\n{combined}"
        );
        assert!(
            combined.contains("W035"),
            "--doc --fix must report that it wrote the file, got:\n{combined}"
        );
        assert!(
            !combined.contains("E032"),
            "post-fix diagnostics must use fresh compiled line numbers, got:\n{combined}"
        );
    }

    #[test]
    fn cli_doc_lint_fix_does_not_alter_non_doc_cddl_source() {
        let dir = std::env::temp_dir().join("cbork_doc_fix_non_doc");
        let _unused = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        std::fs::write(
            dir.join(".rumdl.toml"),
            "[global]\ndisable = [\"MD001\", \"MD063\", \"MD013\", \"MD041\"]\n",
        )
        .expect("write rumdl config");
        let fixture = dir.join("non_doc.cddl");
        std::fs::write(
            &fixture,
            "\
;@ CBORK: Library
;@ CBORK: Export
;! ### widget
;! Public widget that does a thing.
widget = {
  name: tstr,
  count: uint,
}
",
        )
        .expect("write fixture");

        let output = std::process::Command::new(env!("CARGO"))
            .arg("run")
            .arg("--quiet")
            .arg("--bin")
            .arg("cbork")
            .arg("lint")
            .arg("--doc")
            .arg("--fix")
            .arg(&fixture)
            .arg("--no-banner")
            .output()
            .expect("run cbork lint --doc --fix");
        let combined = String::from_utf8_lossy(&output.stdout).into_owned()
            + &String::from_utf8_lossy(&output.stderr);
        // Must read the modified file BEFORE removing the temp dir.
        let modified = std::fs::read_to_string(&fixture).expect("read modified fixture");
        let _unused = std::fs::remove_dir_all(&dir);

        assert!(combined.contains("W035"), "expected W035, got:\n{combined}");
        // The CDDL rule body (multi-line) must be byte-for-byte identical.
        assert!(
            modified.contains("  name: tstr,"),
            "CDDL source lines must be preserved:\n{modified}"
        );
        assert!(
            modified.contains("  count: uint,"),
            "CDDL source lines must be preserved:\n{modified}"
        );
        assert!(
            modified.contains(";@ CBORK: Library"),
            "`;@ CBORK: Library` must remain unchanged:\n{modified}"
        );
    }

    #[test]
    fn safety_validation_rejects_reserved_marker_prefix() {
        let source = "\
;! # Title
;! CBORK CDDL FROM 99-99
rule = 1
";
        let report = cbork_cddl_compiler::validate_doc_source(source);
        assert!(!report.is_clean());
        assert!(
            report.diagnostics.iter().any(|d| d.code == "E040"),
            "expected E040 reserved-marker diagnostic, got: {:#?}",
            report.diagnostics
        );
    }

    #[test]
    fn safety_validation_rejects_unclosed_html_comment() {
        let source = "\
;! # Title
;! <!-- never closed
rule = 1
";
        let report = cbork_cddl_compiler::validate_doc_source(source);
        assert!(!report.is_clean());
        assert!(
            report.diagnostics.iter().any(|d| d.code == "E041"),
            "expected E041 unclosed-comment diagnostic, got: {:#?}",
            report.diagnostics
        );
    }

    #[test]
    fn safety_validation_accepts_clean_fixture() {
        let source =
            read_fixture("cddl/vectors/project/positive/doc_lint/doc_file_with_title.cddl");
        let report = cbork_cddl_compiler::validate_doc_source(&source);
        assert!(
            report.is_clean(),
            "expected clean report, got: {:#?}",
            report.diagnostics
        );
    }

    #[test]
    fn rumdl_integration_runs_against_transformed_synthetic_markdown() {
        // Step 6 → 5 → 7 → 8: full pipeline on a real fixture.
        let source =
            read_fixture("cddl/vectors/project/positive/doc_lint/doc_file_with_title.cddl");
        let path =
            repo_root().join("cddl/vectors/project/positive/doc_lint/doc_file_with_title.cddl");

        // Step 6: safety validation.
        let safety = cbork_cddl_compiler::validate_doc_source(&source);
        assert!(safety.is_clean(), "fixture must be safe: {safety:#?}");

        // Step 5: transform to synthetic Markdown.
        let synthetic = cbork_cddl_compiler::transform_to_markdown(&source);
        assert!(!synthetic.text.is_empty());

        // Step 7: run `rumdl` on the synthetic Markdown in memory.
        // The call must not error and must produce a Vec<LintWarning>
        // (possibly empty). The exact warning set depends on the
        // configured rules; we only assert that the pipeline ran.
        let rumdl_run = cbork_cddl_compiler::lint_synthetic_markdown(&synthetic, &path, None)
            .expect("rumdl integration must succeed on the synthetic markdown");
        let warning_count = rumdl_run.warnings.len();

        // Step 8: each real rumdl warning is mapped back to a CDDL
        // diagnostic or to a suppressed entry. The total must equal
        // the number of warnings we got from rumdl.
        let mapped = cbork_cddl_compiler::map_rumdl_diagnostics(
            rumdl_run.warnings,
            &synthetic,
            &source,
            &path,
        );
        let total = mapped.diagnostics.len() + mapped.suppressed.len();
        assert_eq!(
            total, warning_count,
            "every rumdl warning must be classified as a CDDL diagnostic or suppressed, got: {mapped:#?}"
        );

        // The synthetic Markdown must contain at least one doc line
        // and at least one splice marker so the suppression paths are
        // both reachable.
        assert!(
            synthetic.lines.iter().any(|l| {
                matches!(
                    l.kind,
                    cbork_cddl_compiler::SyntheticLineKind::DocLine { .. }
                )
            }),
            "fixture must produce at least one doc line"
        );
        assert!(
            synthetic.lines.iter().any(|l| {
                matches!(
                    l.kind,
                    cbork_cddl_compiler::SyntheticLineKind::SpliceMarker { .. }
                )
            }),
            "fixture must produce at least one splice marker"
        );
    }

    // Step 8 (recap, feature 001): the new structured per-file and
    // directory-aggregate counters must behave correctly even without
    // touching the printer code paths. The four tests below cover:
    //   * the combined compiler + doc counts relationship
    //   * the syntax-error-as-one-error semantics
    //   * directory aggregates over clean + warning + errored files
    //   * `--strict` at the directory level failing on a warning

    #[test]
    fn lint_file_combines_compiler_and_doc_counts() {
        let path = write_temp_file(
            "lint_combined_counts.cddl",
            ";@ CBORK: Library\nwidget = external-value\n",
        );

        let result = lint_file(
            &path,
            &LintRunOptions::from_flags_and_doc(FLAG_SUMMARY, DocLintOptions {
                enable: true,
                apply_fixes: false,
                doc_internal: cbork_cddl_compiler::DocInternalPolicy::No,
            }),
            FilePrintMode::SingleFile,
        );

        let combined = super::DiagnosticCounts {
            errors: result.compiler_counts.errors + result.doc_counts.errors,
            warnings: result.compiler_counts.warnings + result.doc_counts.warnings,
        };
        assert_eq!(
            result.counts.errors, combined.errors,
            "combined errors must equal compiler + doc errors"
        );
        assert_eq!(
            result.counts.warnings, combined.warnings,
            "combined warnings must equal compiler + doc warnings"
        );

        let _unused = std::fs::remove_file(&path);
    }

    #[test]
    fn syntax_error_counts_as_one_error() {
        let path = write_temp_file(
            "lint_syntax_one_error.cddl",
            "totally = invalid ) cddl syntax\n",
        );

        let result = lint_file(
            &path,
            &LintRunOptions::from_flags_and_doc(0, DocLintOptions::default_off()),
            FilePrintMode::SingleFile,
        );

        assert!(!result.ok, "syntax error must produce ok=false");
        assert!(result.syntax_error, "syntax_error flag must be set");
        assert!(!result.fatal_error, "syntax_error must not be fatal_error");
        assert_eq!(
            result.counts.errors, 1,
            "syntax errors contribute exactly 1 error to the aggregate"
        );
        assert_eq!(result.counts.warnings, 0);
        assert_eq!(
            result.compiler_counts.errors, 0,
            "we have no diagnostic vec for syntax errors, so compiler_counts stays 0"
        );
        assert_eq!(result.doc_counts.errors, 0);
        assert_eq!(
            result.compiler_counts.warnings + result.doc_counts.warnings,
            0
        );

        let _unused = std::fs::remove_file(&path);
    }

    fn build_recursive_aggregate_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(name);
        let _unused = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp lint dir should exist");
        dir
    }

    #[test]
    fn directory_lint_aggregates_file_counts() {
        let dir = build_recursive_aggregate_dir("cbork_lint_aggregate_counts");
        std::fs::write(dir.join("clean.cddl"), "clean = uint\n").expect("write");
        std::fs::write(
            dir.join("warn.cddl"),
            ";@ CBORK: Library\nwidget = external-value\n",
        )
        .expect("write");
        std::fs::write(dir.join("broken.cddl"), "broken ) = cddl\n").expect("write");

        let aggregate = lint_dir(
            &dir,
            &LintRunOptions::from_flags_and_doc(0, DocLintOptions::default_off()),
        );

        assert_eq!(aggregate.files, 3, "three CDDL files were processed");
        assert_eq!(
            aggregate.failed_files, 1,
            "only the syntax-error file is failed"
        );
        assert_eq!(aggregate.clean_files(), 2);
        assert_eq!(
            aggregate.counts.errors, 1,
            "exactly one syntax-error contributes to error count"
        );
        assert!(
            aggregate.counts.warnings >= 1,
            "the unresolved external reference contributes at least one warning"
        );
        assert!(
            !aggregate.ok(),
            "aggregate with one failing file must report ok=false"
        );
    }

    #[test]
    fn directory_lint_counts_failed_and_clean_files() {
        let dir = build_recursive_aggregate_dir("cbork_lint_aggregate_mix");
        std::fs::write(dir.join("clean_a.cddl"), "alpha = uint\n").expect("write");
        std::fs::write(dir.join("clean_b.cddl"), "beta = uint\n").expect("write");
        std::fs::write(dir.join("errored.cddl"), "broken ) = cddl\n").expect("write");

        let aggregate = lint_dir(
            &dir,
            &LintRunOptions::from_flags_and_doc(0, DocLintOptions::default_off()),
        );

        assert_eq!(aggregate.files, 3);
        assert_eq!(aggregate.failed_files, 1);
        assert_eq!(aggregate.clean_files(), 2);
        assert_eq!(aggregate.counts.errors, 1);
        assert_eq!(aggregate.counts.warnings, 0);

        assert!(
            LintAggregateResult {
                files: 0,
                failed_files: 0,
                counts: super::DiagnosticCounts::default(),
            }
            .ok()
        );
        assert!(
            !LintAggregateResult {
                files: 5,
                failed_files: 1,
                counts: super::DiagnosticCounts::default(),
            }
            .ok()
        );
    }

    #[test]
    fn strict_directory_lint_fails_on_warning() {
        let dir = build_recursive_aggregate_dir("cbork_strict_dir_lint");
        std::fs::write(
            dir.join("warning.cddl"),
            ";@ CBORK: Library\nwidget = external-value\n",
        )
        .expect("write");

        let aggregate = lint_dir(
            &dir,
            &LintRunOptions::from_flags_and_doc(
                FLAG_FAIL_ON_WARNINGS,
                DocLintOptions::default_off(),
            ),
        );

        assert_eq!(aggregate.files, 1);
        assert_eq!(
            aggregate.failed_files, 1,
            "a warning-only file under --strict must be counted as failed"
        );
        assert_eq!(aggregate.clean_files(), 0);
        assert!(
            !aggregate.ok(),
            "the --strict flag must propagate to the aggregate ok() result"
        );
    }

    // CLI integration: a recursive `--doc --summary` run must
    //   * print exactly one final summary line (the "Recursive lint summary:")
    //   * print zero per-file `Summary:` lines for the items inside the directory
    //   * preserve exit code semantics that fail on errors / --strict warnings
    //
    // The test drives the actual `cbork` binary through `cargo run`, the
    // same pattern the existing `cli_doc_lint_*` tests use, so it sees
    // the real stdout without any test-only stdout capture shim.

    fn run_lint_cli(
        rel: &str,
        extra_args: &[&str],
    ) -> (i32, String) {
        let path = repo_root().join(rel);
        let mut command = std::process::Command::new(env!("CARGO"));
        command
            .arg("run")
            .arg("--quiet")
            .arg("--bin")
            .arg("cbork")
            .arg("lint")
            .args(extra_args)
            .arg("--no-banner")
            .arg(&path);
        let output = command.output().expect("run cbork lint");
        let exit_code = output.status.code().unwrap_or(-1);
        let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
        combined.push_str(&String::from_utf8_lossy(&output.stderr));
        (exit_code, combined)
    }

    #[test]
    fn cli_recursive_lint_summary_is_single_line() {
        let (code, output) = run_lint_cli("cddl/vectors/project/semantic-errors", &[
            "--recursive",
            "--summary",
        ]);
        assert_eq!(
            code, 1,
            "semantic-errors fixtures contain errors, exit must be 1, got {code}:\n{output}"
        );
        let summary_lines: Vec<_> = output
            .lines()
            .filter(|line| line.starts_with("Summary:"))
            .collect();
        assert_eq!(
            summary_lines.len(),
            0,
            "recursive --summary must not emit any per-file Summary: lines, got:\n{output}"
        );
        let recursive_count = output
            .lines()
            .filter(|line| line.contains("Recursive lint summary:"))
            .count();
        assert_eq!(
            recursive_count, 1,
            "exactly one final Recursive lint summary: line is required, got {recursive_count}:\n{output}"
        );
    }

    #[test]
    fn cli_recursive_doc_lint_summary_is_single_line() {
        let (code, output) = run_lint_cli("cddl/vectors/project/positive/doc_lint", &[
            "--recursive",
            "--doc",
            "--summary",
        ]);
        assert_eq!(
            code, 0,
            "positive doc_lint fixtures pass under --doc, exit must be 0, got {code}:\n{output}"
        );
        let per_file_count = output
            .lines()
            .filter(|line| line.starts_with("Summary:"))
            .count();
        assert_eq!(
            per_file_count, 0,
            "recursive --doc --summary must not emit any per-file Summary: lines, got:\n{output}"
        );
        let recursive_count = output
            .lines()
            .filter(|line| line.contains("Recursive lint summary:"))
            .count();
        assert_eq!(
            recursive_count, 1,
            "exactly one final Recursive lint summary: line is required, got {recursive_count}:\n{output}"
        );
    }

    // `--no-fail` is a global CLI switch (plans/plan-006-no-fail.md) that
    // forces the process exit code to 0 even when a subcommand would
    // normally report a failure. It must not suppress diagnostics or
    // change the visible command output.

    fn run_cbork(args: &[&str]) -> (i32, String) {
        let output = std::process::Command::new(env!("CARGO"))
            .arg("run")
            .arg("--quiet")
            .arg("--bin")
            .arg("cbork")
            .arg("--")
            .args(args)
            .arg("--no-banner")
            .output()
            .expect("run cbork");
        let exit_code = output.status.code().unwrap_or(-1);
        let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
        combined.push_str(&String::from_utf8_lossy(&output.stderr));
        (exit_code, combined)
    }

    fn write_no_fail_cddl(
        name: &str,
        content: &str,
    ) -> PathBuf {
        let dir = std::env::temp_dir().join("cbork_no_fail_test");
        std::fs::create_dir_all(&dir).expect("temp no-fail dir should exist");
        let path = dir.join(name);
        std::fs::write(&path, content).expect("temp no-fail file should be written");
        path
    }

    #[test]
    fn cli_without_no_fail_still_returns_nonzero_for_syntax_error() {
        let path = write_no_fail_cddl("no_fail_syntax.cddl", "totally = invalid ) cddl syntax\n");
        let (code, output) = run_cbork(&["lint", path.to_str().expect("utf-8 path")]);
        assert_ne!(
            code, 0,
            "lint of a broken file without --no-fail must be non-zero, got {code}:\n{output}"
        );
    }

    #[test]
    fn cli_no_fail_lint_returns_zero_for_syntax_error() {
        let path = write_no_fail_cddl(
            "no_fail_syntax_ok.cddl",
            "totally = invalid ) cddl syntax\n",
        );
        let (code, output) = run_cbork(&["--no-fail", "lint", path.to_str().expect("utf-8 path")]);
        assert_eq!(
            code, 0,
            "--no-fail lint of a broken file must still exit 0, got {code}:\n{output}"
        );
    }

    #[test]
    fn cli_no_fail_preserves_lint_diagnostics() {
        let path = write_no_fail_cddl("no_fail_diag.cddl", "totally = invalid ) cddl syntax\n");
        let (code, output) = run_cbork(&["--no-fail", "lint", path.to_str().expect("utf-8 path")]);
        assert_eq!(code, 0, "expected exit 0, got {code}:\n{output}");
        assert!(
            output.contains("syntax error"),
            "--no-fail must still print the syntax error diagnostic, got:\n{output}"
        );
        assert!(
            output.contains("expected EOI"),
            "--no-fail must still include the parser detail, got:\n{output}"
        );
    }

    #[test]
    fn cli_no_fail_unknown_why_code_returns_zero() {
        let (code, output) = run_cbork(&["--no-fail", "why", "Z9999"]);
        assert_eq!(
            code, 0,
            "--no-fail why for an unknown code must exit 0, got {code}:\n{output}"
        );
        assert!(
            output.contains("unknown diagnostic code"),
            "--no-fail must still print the unknown-code message, got:\n{output}"
        );

        let (code, output) = run_cbork(&["why", "Z9999"]);
        assert_ne!(
            code, 0,
            "why for an unknown code without --no-fail must be non-zero, got {code}:\n{output}"
        );
    }

    #[test]
    fn cli_agent_skills_check_empty_fails() {
        let dest =
            std::env::temp_dir().join(format!("cbork_agent_skills_empty_{}", std::process::id()));
        drop(std::fs::remove_dir_all(&dest));
        let (code, output) = run_cbork(&[
            "agent-skills",
            "--check",
            dest.to_str().expect("utf-8 temp path"),
        ]);
        assert_ne!(
            code, 0,
            "--check on an empty dir must be non-zero, got {code}:\n{output}"
        );
        assert!(
            output.contains("differ") || output.contains("false"),
            "output should indicate failure state:\n{output}"
        );
    }

    #[test]
    fn cli_agent_skills_check_no_fail_returns_zero() {
        let dest =
            std::env::temp_dir().join(format!("cbork_agent_skills_nf_{}", std::process::id()));
        drop(std::fs::remove_dir_all(&dest));
        let (code, output) = run_cbork(&[
            "--no-fail",
            "agent-skills",
            "--check",
            dest.to_str().expect("utf-8 temp path"),
        ]);
        assert_eq!(
            code, 0,
            "--no-fail --check on stale must exit 0, got {code}:\n{output}"
        );
    }

    #[test]
    fn cli_agent_skills_install_then_check_passes() {
        let dest =
            std::env::temp_dir().join(format!("cbork_agent_skills_install_{}", std::process::id()));
        drop(std::fs::remove_dir_all(&dest));

        let dest_str = dest.to_str().expect("utf-8 temp path");
        let (code, output) = run_cbork(&["agent-skills", dest_str]);
        assert_eq!(code, 0, "install must succeed, got {code}:\n{output}");

        let (code, output) = run_cbork(&["agent-skills", "--check", dest_str]);
        assert_eq!(
            code, 0,
            "--check after install must succeed, got {code}:\n{output}"
        );
    }

    #[test]
    fn cli_agent_skills_overwrite_works() {
        let dest =
            std::env::temp_dir().join(format!("cbork_agent_skills_ow_{}", std::process::id()));
        drop(std::fs::remove_dir_all(&dest));

        let dest_str = dest.to_str().expect("utf-8 temp path");
        // First install normally
        let (code, _) = run_cbork(&["agent-skills", dest_str]);
        assert_eq!(code, 0, "first install must succeed");

        // Overwrite (should succeed since files are identical)
        let (code, _) = run_cbork(&["agent-skills", "--overwrite", dest_str]);
        assert_eq!(code, 0, "--overwrite after install must succeed");
    }

    // Plan 002 — recursive discovery honors ignore files.
    //
    // These tests build a temp tree that mirrors the fixture layout in
    // `plans/plan-002-recursive-exclusion.md`:
    //
    //     temp/
    //       .gitignore       -> ignores `generated/`
    //       .ignore          -> ignores `ignored-by-nested.cddl`
    //       included.cddl
    //       generated/ignored.cddl
    //       nested/.gitignore
    //       nested/ignored-by-nested.cddl
    //       nested/included-nested.cddl
    //       .hidden/hidden.cddl
    //       .git/must-never-lint.cddl
    //
    // Default discovery should return `included.cddl` and
    // `nested/included-nested.cddl` and nothing else.

    fn build_discovery_fixture(root: &Path) {
        std::fs::create_dir_all(root).expect("discovery root should exist");
        std::fs::write(root.join(".gitignore"), "generated/\n")
            .expect(".gitignore should be written");
        std::fs::write(root.join(".ignore"), "ignored-by-nested.cddl\n")
            .expect(".ignore should be written");
        std::fs::write(root.join("included.cddl"), "included = uint\n")
            .expect("included.cddl should be written");

        std::fs::create_dir_all(root.join("generated")).expect("generated/ should exist");
        std::fs::write(
            root.join("generated").join("ignored.cddl"),
            "broken ) = cddl\n",
        )
        .expect("generated/ignored.cddl should be written");

        std::fs::create_dir_all(root.join("nested")).expect("nested/ should exist");
        std::fs::write(
            root.join("nested").join(".gitignore"),
            "ignored-by-nested.cddl\n",
        )
        .expect("nested/.gitignore should be written");
        std::fs::write(
            root.join("nested").join("ignored-by-nested.cddl"),
            "broken ) = cddl\n",
        )
        .expect("nested/ignored-by-nested.cddl should be written");
        std::fs::write(
            root.join("nested").join("included-nested.cddl"),
            "included-nested = uint\n",
        )
        .expect("nested/included-nested.cddl should be written");

        std::fs::create_dir_all(root.join(".hidden")).expect(".hidden/ should exist");
        std::fs::write(
            root.join(".hidden").join("hidden.cddl"),
            "broken ) = cddl\n",
        )
        .expect(".hidden/hidden.cddl should be written");

        std::fs::create_dir_all(root.join(".git")).expect(".git/ should exist");
        std::fs::write(
            root.join(".git").join("must-never-lint.cddl"),
            "broken ) = cddl\n",
        )
        .expect(".git/must-never-lint.cddl should be written");
    }

    fn fresh_discovery_root(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cbork_recursive_discovery_{label}"));
        drop(std::fs::remove_dir_all(&dir));
        dir
    }

    #[test]
    fn recursive_discovery_respects_gitignore_by_default() {
        let root = fresh_discovery_root("gitignore_default");
        build_discovery_fixture(&root);

        let files =
            discover_cddl_files(&root, RecursiveScanOptions::default_off()).expect("discovery");

        let relative: Vec<String> = files
            .iter()
            .map(|path| {
                path.strip_prefix(&root)
                    .map_or_else(|_| path.display().to_string(), |p| p.display().to_string())
            })
            .collect();

        assert!(
            relative.iter().all(|name| !name.contains("generated")),
            "default discovery must skip generated/ via .gitignore, got {relative:?}"
        );
        assert!(
            !relative.iter().any(|name| name.ends_with("ignored.cddl")),
            "default discovery must skip generated/ignored.cddl via .gitignore, got {relative:?}"
        );
    }

    #[test]
    fn recursive_discovery_respects_ignore_files_by_default() {
        let root = fresh_discovery_root("ignore_default");
        build_discovery_fixture(&root);

        let files =
            discover_cddl_files(&root, RecursiveScanOptions::default_off()).expect("discovery");

        let relative: Vec<String> = files
            .iter()
            .map(|path| {
                path.strip_prefix(&root)
                    .map_or_else(|_| path.display().to_string(), |p| p.display().to_string())
            })
            .collect();

        assert!(
            !relative
                .iter()
                .any(|name| name.ends_with("ignored-by-nested.cddl")),
            "default discovery must skip nested/ignored-by-nested.cddl via .ignore, got {relative:?}"
        );
    }

    #[test]
    fn recursive_discovery_no_ignore_includes_ignore_matches() {
        let root = fresh_discovery_root("no_ignore");
        build_discovery_fixture(&root);

        let opts = RecursiveScanOptions {
            respect_ignore_files: false,
            include_hidden: false,
        };
        let aggregate = lint_dir(
            &root,
            &LintRunOptions::from_flags_doc_and_scan(0, DocLintOptions::default_off(), opts),
        );

        assert!(
            !aggregate.ok(),
            "expected lint to fail once ignored files are re-included, got aggregate {aggregate:?}"
        );
        assert!(
            aggregate.failed_files >= 2,
            "expected generated/ignored.cddl and nested/ignored-by-nested.cddl to fail, got {aggregate:?}"
        );
    }

    #[test]
    fn recursive_discovery_respects_nested_gitignore() {
        // Place the fixture inside the workspace under a gitignored
        // target subdirectory so the `ignore` crate walker walks up and
        // loads the workspace's own `.gitignore` and `.git` context.
        // The nested `.gitignore` inside the fixture then excludes
        // `nested/ignored.cddl` while `nested/included-nested.cddl`
        // remains visible.
        let root = repo_root()
            .join("target")
            .join("cbork_recursive_discovery_nested_gitignore");
        drop(std::fs::remove_dir_all(&root));
        std::fs::create_dir_all(root.join("nested")).expect("nested/ should exist");
        std::fs::write(root.join("nested").join(".gitignore"), "ignored.cddl\n")
            .expect("nested/.gitignore should be written");
        std::fs::write(
            root.join("nested").join("ignored.cddl"),
            "broken ) = cddl\n",
        )
        .expect("nested/ignored.cddl should be written");
        std::fs::write(
            root.join("nested").join("included-nested.cddl"),
            "included-nested = uint\n",
        )
        .expect("nested/included-nested.cddl should be written");

        let files =
            discover_cddl_files(&root, RecursiveScanOptions::default_off()).expect("discovery");
        let names: Vec<String> = files
            .iter()
            .map(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_owned)
                    .expect("utf-8 filename")
            })
            .collect();

        assert_eq!(
            names,
            vec!["included-nested.cddl".to_owned()],
            "nested .gitignore must exclude ignored.cddl, got {names:?}"
        );
    }

    #[test]
    fn recursive_discovery_hidden_flag_includes_hidden_entries() {
        let root = fresh_discovery_root("hidden");
        build_discovery_fixture(&root);

        let default_files =
            discover_cddl_files(&root, RecursiveScanOptions::default_off()).expect("discovery");
        assert!(
            !default_files
                .iter()
                .any(|path| path.ends_with(".hidden/hidden.cddl")),
            "default discovery must skip .hidden/, got {default_files:?}"
        );

        let hidden_opts = RecursiveScanOptions {
            respect_ignore_files: true,
            include_hidden: true,
        };
        let hidden_files = discover_cddl_files(&root, hidden_opts).expect("discovery");
        assert!(
            hidden_files
                .iter()
                .any(|path| path.ends_with(".hidden/hidden.cddl")),
            "--hidden must include .hidden/hidden.cddl, got {hidden_files:?}"
        );
    }

    #[test]
    fn recursive_discovery_always_skips_git_directory() {
        let root = fresh_discovery_root("always_skip_git");
        build_discovery_fixture(&root);

        // Even with --hidden and --no-ignore combined, the .git/ directory
        // must remain excluded by the entry filter.
        let opts = RecursiveScanOptions {
            respect_ignore_files: false,
            include_hidden: true,
        };
        let files = discover_cddl_files(&root, opts).expect("discovery");

        assert!(
            !files.iter().any(|path| path.starts_with(root.join(".git"))),
            ".git/ must never be traversed, got {files:?}"
        );
        assert!(
            !files
                .iter()
                .any(|path| path.ends_with("must-never-lint.cddl")),
            ".git/must-never-lint.cddl must never be discovered, got {files:?}"
        );
    }

    #[test]
    fn explicit_ignored_file_still_lints() {
        // Lint an explicit file path even when it would be skipped during
        // recursive discovery because it is matched by .ignore.
        let root = fresh_discovery_root("explicit_ignored");
        build_discovery_fixture(&root);
        let target = root.join("nested").join("ignored-by-nested.cddl");

        let ok = check_file_with_print(
            &target,
            &LintRunOptions::from_flags_and_doc(0, DocLintOptions::default_off()),
        );
        assert!(
            !ok,
            "explicit ignored file should still be linted and report its parse error"
        );
    }

    // Plan 016 — `cbork decode` CLI tests covering the schema-free decode
    // mode and the opt-in `--try-cbor-bstr` flag.

    /// Build a temporary CBOR file from a byte vector and return the path.
    fn write_temp_cbor(
        name: &str,
        bytes: &[u8],
    ) -> PathBuf {
        let dir = std::env::temp_dir().join("cbork_decode_cli_test");
        std::fs::create_dir_all(&dir).expect("temp decode-cli dir should exist");
        let path = dir.join(name);
        std::fs::write(&path, bytes).expect("temp cbor file should be written");
        path
    }

    /// `cbork decode` without any flag must render a CBOR byte string as
    /// `h'...'` exactly as before plan 016 (the flag must be the only
    /// behavior change).
    #[test]
    fn cli_decode_plain_byte_string_keeps_raw_h_literal() {
        // A top-level byte string `h'a1 01 26'` is encoded as `0x43` (a
        // byte string of length 3) followed by the bytes `a1 01 26`.
        let bytes = [0x43, 0xA1, 0x01, 0x26];
        let path = write_temp_cbor("decode_plain.cbor", &bytes);
        let (code, output) =
            run_cbork(&["decode", "--no-color", path.to_str().expect("utf-8 path")]);
        assert_eq!(code, 0, "decode must succeed, got {code}:\n{output}");
        assert!(
            output.contains("h'a1 01 26'"),
            "plain decode must keep the raw byte string:\n{output}"
        );
    }

    /// `cbork decode --try-cbor-bstr` must expand byte strings that parse
    /// as CBOR into the EDN-literals draft's `<<...>>` wrapper.
    #[test]
    fn cli_decode_try_cbor_bstr_expands_inner_cbor() {
        // `{"protected": h'a10126', "unprotected": {}}` — a COSE-like map.
        // Each text string carries its own length prefix in CBOR.
        let mut bytes = vec![0xA2];
        // key: text string of 9 bytes "protected"
        bytes.push(0x69);
        bytes.extend_from_slice(b"protected");
        // value: byte string of 3 bytes
        bytes.extend_from_slice(&[0x43, 0xA1, 0x01, 0x26]);
        // key: text string of 11 bytes "unprotected"
        bytes.push(0x6B);
        bytes.extend_from_slice(b"unprotected");
        // value: empty map
        bytes.push(0xA0);
        let path = write_temp_cbor("decode_try_cbor.cbor", &bytes);
        let (code, output) = run_cbork(&[
            "decode",
            "--no-color",
            "--try-cbor-bstr",
            path.to_str().expect("utf-8 path"),
        ]);
        assert_eq!(code, 0, "decode must succeed, got {code}:\n{output}");
        assert!(
            output.contains("<<"),
            "try-cbor-bstr must emit `<<` wrapper:\n{output}"
        );
        assert!(
            output.contains(">>"),
            "try-cbor-bstr must emit `>>` wrapper:\n{output}"
        );
        assert!(
            output.contains("1: -7"),
            "try-cbor-bstr must expose the decoded value:\n{output}"
        );
        // The raw byte-string form is replaced.
        assert!(
            !output.contains("h'a1 01 26'"),
            "raw bytes must not appear once expanded:\n{output}"
        );
    }

    /// `cbork decode --try-cbor-bstr` must keep empty byte strings as
    /// `h''` because empty embedded sequences are reserved for explicit
    /// `.cborseq`/`.prefpseq`/`.dtrmseq` schema contexts.
    #[test]
    fn cli_decode_try_cbor_bstr_keeps_empty_byte_string() {
        // `0x40` is the empty byte string.
        let path = write_temp_cbor("decode_try_empty.cbor", &[0x40]);
        let (code, output) = run_cbork(&[
            "decode",
            "--no-color",
            "--try-cbor-bstr",
            path.to_str().expect("utf-8 path"),
        ]);
        assert_eq!(code, 0, "decode must succeed, got {code}:\n{output}");
        assert!(
            output.contains("h''"),
            "empty byte string must stay raw under try-cbor-bstr:\n{output}"
        );
        assert!(
            !output.contains("<<"),
            "empty byte string must not produce a wrapper:\n{output}"
        );
    }

    /// `cbork decode --try-cbor-bstr` must keep non-CBOR bytes raw without
    /// emitting a warning.
    #[test]
    fn cli_decode_try_cbor_bstr_keeps_non_cbor_bytes_raw() {
        // `0x43 0xFF 0xFF 0xFF` is a 3-byte string whose bytes are not
        // a valid CBOR item.
        let path = write_temp_cbor("decode_try_invalid.cbor", &[0x43, 0xFF, 0xFF, 0xFF]);
        let (code, output) = run_cbork(&[
            "decode",
            "--no-color",
            "--try-cbor-bstr",
            path.to_str().expect("utf-8 path"),
        ]);
        assert_eq!(code, 0, "decode must succeed, got {code}:\n{output}");
        assert!(
            output.contains("h'ff ff ff'"),
            "non-CBOR bytes must stay raw under try-cbor-bstr:\n{output}"
        );
    }

    // Plan 017 regression — the exact reproduction command from the
    // plan. The dntls ML-KEM-768-X25519 public-key vector must
    // validate as bytes against the ABNF grammar declared in
    // dntls-cose-defs.cddl. We use the detailed dump so the
    // CDN-comment breakdown is exercised end-to-end.
    #[test]
    fn cli_validate_dntls_mlkem768_x25519_public_key_succeeds() {
        let workspace = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .unwrap()
            .to_path_buf();
        let schema = workspace.join("test/dntls-core/doc/dntls-cose-defs.cddl");
        let vector = workspace.join("test/dntls-core/vectors/mlkem768_public.cbor");
        let (code, output) = run_cbork(&[
            "--color=never",
            "validate",
            "--type=Cose-Public-Key-mlkem768-x25519-dntls",
            "--detailed",
            schema.to_str().expect("utf-8 path"),
            vector.to_str().expect("utf-8 path"),
        ]);
        assert_eq!(
            code, 0,
            "reproduction command must succeed, got {code}:\n{output}"
        );
        assert!(
            output.contains(": OK"),
            "reproduction must report a successful match:\n{output}"
        );
        // The detailed dump must include the CDN comment breakdown.
        assert!(
            output.contains("// ABNF:"),
            "detailed dump must include the ABNF comment breakdown:\n{output}"
        );
        assert!(
            output.contains("ml-kem-768-x25519-public-key"),
            "comment breakdown must include the start rule:\n{output}"
        );
        assert!(
            output.contains("ml-kem-768-public-key"),
            "comment breakdown must include the 1184-byte child rule:\n{output}"
        );
        assert!(
            output.contains("x25519-public-key"),
            "comment breakdown must include the 32-byte child rule:\n{output}"
        );
    }
}
