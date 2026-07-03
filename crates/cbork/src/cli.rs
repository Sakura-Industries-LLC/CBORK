// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: AGPL-3.0-only

//! Command-line interface for the `cbork` tool.
//!
//! This module owns CLI parsing and dispatch only.

use std::{fmt, path::PathBuf, process::exit, str::FromStr};

use bpaf::{Bpaf, Parser, choice};
use cbork_cddl_compiler::CompiledCDDL;
use console::{Emoji, style};

use crate::{
    decode,
    diagnostics::{has_error_diagnostics, print_compiler_diagnostics},
    lint, render, rfc, ui, validate, why, xref,
};

/// Global `cbork` options.
#[derive(Debug, Clone, Bpaf)]
#[bpaf(options, version)]
pub(crate) struct Cli {
    /// Color policy for terminal output.
    #[bpaf(long, argument("auto|always|never"), fallback(ColorMode::Auto))]
    color: ColorMode,

    /// Suppress non-essential output.
    #[bpaf(long)]
    quiet: bool,

    /// Suppress the startup banner.
    #[bpaf(long)]
    no_banner: bool,

    /// Increase diagnostic verbosity.
    #[bpaf(short, long)]
    verbose: bool,

    /// Output format for machine-oriented workflows.
    #[bpaf(long, argument("rich|plain|json"), fallback(OutputFormat::Rich))]
    format: OutputFormat,

    /// Optional configuration file for future expansion.
    #[bpaf(long, argument("PATH"))]
    config: Option<PathBuf>,

    /// Selected cbork subcommand.
    #[bpaf(external(command))]
    command: Command,
}

impl Cli {
    /// Execute the selected command.
    pub(crate) fn exec(self) {
        let _ = (self.color, self.verbose, self.config.as_ref());
        let show_banner =
            !self.quiet && !self.no_banner && matches!(self.format, OutputFormat::Rich);
        if show_banner {
            ui::print_banner();
        }

        let ok = match self.command {
            Command::Lint(args) => args.exec(),
            Command::Compile(args) => args.exec(),
            Command::Why(args) => args.exec(),
            Command::Xref(args) => args.exec(),
            Command::Rfc(args) => args.exec(),
            Command::Fmt(args) => args.exec_stub("fmt"),
            Command::Render(args) => args.exec(),
            Command::Decode(args) => args.exec(matches!(self.color, ColorMode::Never)),
            Command::Validate(args) => args.exec(matches!(self.color, ColorMode::Never)),
            Command::Explain(args) => args.exec_stub("explain"),
            Command::Coverage(args) => args.exec_stub("coverage"),
            Command::Docs(args) => args.exec_stub("docs"),
            Command::Lsp(args) => args.exec_stub("lsp"),
        };

        if !ok {
            exit(1);
        }
    }
}

/// The selected subcommand and its parsed arguments.
#[derive(Debug, Clone)]
enum Command {
    /// Lint one or more CDDL sources.
    Lint(Lint),
    /// Inspect the current compiler pipeline output.
    Compile(Compile),
    /// Explain why a diagnostic exists.
    Why(Why),
    /// Cross-reference a standards topic.
    Xref(Xref),
    /// Dump an embedded RFC or list the embedded corpus.
    Rfc(Rfc),
    /// Format CDDL source.
    Fmt(Fmt),
    /// Render an expanded schema view.
    Render(Render),
    /// Decode CBOR input.
    Decode(Decode),
    /// Validate CBOR against a schema.
    Validate(Validate),
    /// Explain schema meaning.
    Explain(Explain),
    /// Report schema coverage.
    Coverage(Coverage),
    /// Emit schema documentation.
    Docs(Docs),
    /// Launch the language server.
    Lsp(Lsp),
}

/// Build the command parser from all supported subcommands.
fn command() -> impl Parser<Command> {
    choice(vec![
        boxed_command(lint().map(Command::Lint)),
        boxed_command(compile().map(Command::Compile)),
        boxed_command(why().map(Command::Why)),
        boxed_command(xref().map(Command::Xref)),
        boxed_command(rfc().map(Command::Rfc)),
        boxed_command(fmt().map(Command::Fmt)),
        boxed_command(render().map(Command::Render)),
        boxed_command(decode().map(Command::Decode)),
        boxed_command(validate().map(Command::Validate)),
        boxed_command(explain().map(Command::Explain)),
        boxed_command(coverage().map(Command::Coverage)),
        boxed_command(docs().map(Command::Docs)),
        boxed_command(lsp().map(Command::Lsp)),
    ])
}

/// Box a command parser for `bpaf::choice`.
fn boxed_command(parser: impl Parser<Command> + 'static) -> Box<dyn Parser<Command>> {
    Box::new(parser)
}

/// Color handling policy for CLI output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ColorMode {
    /// Use terminal-detected colors.
    Auto,
    /// Always emit color escape sequences.
    Always,
    /// Never emit color escape sequences.
    Never,
}

impl fmt::Display for ColorMode {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        f.write_str(match self {
            Self::Auto => "auto",
            Self::Always => "always",
            Self::Never => "never",
        })
    }
}

impl FromStr for ColorMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "auto" => Ok(Self::Auto),
            "always" => Ok(Self::Always),
            "never" => Ok(Self::Never),
            other => Err(format!("unsupported color mode: {other}")),
        }
    }
}

/// Output formatting mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputFormat {
    /// Rich terminal output.
    Rich,
    /// Plain text output.
    Plain,
    /// JSON output.
    Json,
}

impl fmt::Display for OutputFormat {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        f.write_str(match self {
            Self::Rich => "rich",
            Self::Plain => "plain",
            Self::Json => "json",
        })
    }
}

impl FromStr for OutputFormat {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "rich" => Ok(Self::Rich),
            "plain" => Ok(Self::Plain),
            "json" => Ok(Self::Json),
            other => Err(format!("unsupported output format: {other}")),
        }
    }
}

/// Lint command arguments.
#[derive(Debug, Clone, Bpaf)]
#[bpaf(command)]
#[allow(clippy::struct_excessive_bools)]
struct Lint {
    /// Read from standard input.
    #[bpaf(long)]
    stdin: bool,

    /// Recurse through directories.
    #[bpaf(long)]
    recursive: bool,

    /// Attempt fixable lint classes.
    #[bpaf(long)]
    fix: bool,

    /// Enable stricter lint policy.
    #[bpaf(long)]
    strict: bool,

    /// Emit warnings for the named rule.
    #[bpaf(long, argument("RULE"))]
    warn: Vec<String>,

    /// Deny the named rule.
    #[bpaf(long, argument("RULE"))]
    deny: Vec<String>,

    /// Allow the named rule.
    #[bpaf(long, argument("RULE"))]
    allow: Vec<String>,

    /// Emit JSON output.
    #[bpaf(long)]
    json: bool,

    /// Print lint statistics.
    #[bpaf(long)]
    stats: bool,

    /// Print only diagnostic counts.
    #[bpaf(long)]
    summary: bool,

    /// Print standards rationale blocks under each diagnostic.
    #[bpaf(long)]
    why: bool,

    /// Treat the input schema as a reusable library module.
    #[bpaf(long)]
    library: bool,

    /// Run the optional documentation linting pass (`--doc`).
    #[bpaf(long)]
    doc: bool,

    /// Policy for documentation of internal definitions
    /// (`--doc-internal no|warn|yes`). Defaults to `no` so enabling
    /// `--doc` does not force every private helper rule to be
    /// documented immediately.
    #[bpaf(long, argument("no|warn|yes"), fallback(DocLintPolicy::No))]
    doc_internal: DocLintPolicy,

    /// Path to a CDDL file or directory.
    #[bpaf(positional("PATH"))]
    path: PathBuf,
}

/// Policy for documentation of internal (non-exported) definitions,
/// driven by the `--doc-internal` CLI flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DocLintPolicy {
    /// Do not require documentation for internal definitions.
    No,
    /// Warn when an internal definition has no documentation.
    Warn,
    /// Error when an internal definition has no documentation.
    Yes,
}

impl std::str::FromStr for DocLintPolicy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "no" => Ok(Self::No),
            "warn" => Ok(Self::Warn),
            "yes" => Ok(Self::Yes),
            other => Err(format!("unsupported --doc-internal value: {other}")),
        }
    }
}

impl From<DocLintPolicy> for cbork_cddl_compiler::DocInternalPolicy {
    fn from(value: DocLintPolicy) -> Self {
        match value {
            DocLintPolicy::No => Self::No,
            DocLintPolicy::Warn => Self::Warn,
            DocLintPolicy::Yes => Self::Yes,
        }
    }
}

impl Lint {
    /// Execute the lint command using the existing implementation.
    fn exec(self) -> bool {
        let _ = self.stdin
            || self.recursive
            || self.fix
            || self.strict
            || self.json
            || self.stats
            || self.summary
            || self.why
            || self.library
            || self.doc
            || !self.warn.is_empty()
            || !self.deny.is_empty()
            || !self.allow.is_empty();
        let mut flags = 0u8;
        if self.stats {
            flags |= lint::FLAG_STATS;
        }
        if self.summary {
            flags |= lint::FLAG_SUMMARY;
        }
        if self.why {
            flags |= lint::FLAG_WHY;
        }
        if self.strict {
            flags |= lint::FLAG_FAIL_ON_WARNINGS;
        }
        let doc = lint::DocLintOptions {
            enable: self.doc,
            apply_fixes: self.doc && self.fix,
            doc_internal: self.doc_internal.into(),
        };
        let opts = lint::LintRunOptions::from_flags_and_doc(flags, doc);
        if self.path.is_file() {
            lint::check_file_with_print(&self.path, &opts)
        } else {
            lint::check_dir_with_print(&self.path, &opts)
        }
    }
}

/// Compile command arguments.
#[derive(Debug, Clone, Bpaf)]
#[bpaf(command)]
#[allow(clippy::struct_excessive_bools)]
struct Compile {
    /// Dump only user-authored nodes.
    #[bpaf(long)]
    dump_user: bool,

    /// Dump the complete compiled tree.
    #[bpaf(long)]
    dump_complete: bool,

    /// Dump cached compilation state.
    #[bpaf(long)]
    dump_cache: bool,

    /// Suppress tree output.
    #[bpaf(long)]
    no_tree: bool,

    /// Treat the input schema as a reusable library module.
    #[bpaf(long)]
    library: bool,

    /// Emit JSON output.
    #[bpaf(long)]
    json: bool,

    /// Path to a single CDDL source file.
    #[bpaf(positional("PATH"))]
    path: PathBuf,
}

impl Compile {
    /// Execute the compile command using the existing implementation.
    fn exec(self) -> bool {
        let _ = self.dump_user
            || self.dump_complete
            || self.dump_cache
            || self.no_tree
            || self.library
            || self.json;
        compile_file_with_print(&self.path)
    }
}

/// Explain why a diagnostic code exists.
#[derive(Debug, Clone, Bpaf)]
#[bpaf(command)]
struct Why {
    /// Diagnostic code to explain.
    #[bpaf(positional("CODE"))]
    code: Vec<String>,
}

impl Why {
    /// Execute the why command.
    fn exec(self) -> bool {
        if self.code.is_empty() {
            println!("{}", style("Known diagnostic rationales").bold().cyan());
            for entry in why::all() {
                println!("{:<8} {}", entry.code, entry.summary);
            }
            return true;
        }

        let mut ok = true;
        for code in self.code {
            if let Some(entry) = why::find(&code) {
                println!(
                    "{}",
                    style(format!("{}: {}", entry.code, entry.summary))
                        .bold()
                        .cyan()
                );
                print!(
                    "{}",
                    style(rfc::render_citations("WHY", entry.citations)).cyan()
                );
            } else {
                println!(
                    "{}",
                    style(format!("unknown diagnostic code: {code}")).red()
                );
                ok = false;
            }
        }
        ok
    }
}

/// Cross-reference a standards term to the embedded corpus.
#[derive(Debug, Clone, Bpaf)]
#[bpaf(command)]
struct Xref {
    /// Term or operator to look up.
    #[bpaf(positional("QUERY"))]
    query: Vec<String>,
}

impl Xref {
    /// Execute the xref command.
    fn exec(self) -> bool {
        if self.query.is_empty() {
            println!(
                "{}",
                style("Known standards cross-references").bold().cyan()
            );
            for entry in xref::all() {
                println!("{:<24} {}", entry.key, entry.summary);
            }
            return true;
        }

        let mut ok = true;
        for query in self.query {
            let matches = xref::find(&query);
            if matches.is_empty() {
                println!(
                    "{}",
                    style(format!("no xref entries matched: {query}")).red()
                );
                ok = false;
                continue;
            }

            for entry in matches {
                println!(
                    "{}",
                    style(format!("{}: {}", entry.key, entry.summary))
                        .bold()
                        .cyan()
                );
                print!(
                    "{}",
                    style(rfc::render_citations("XREF", entry.citations)).cyan()
                );
            }
        }
        ok
    }
}

/// Dump an embedded RFC or list the embedded corpus.
#[derive(Debug, Clone, Bpaf)]
#[bpaf(command)]
struct Rfc {
    /// Optional embedded document identifier.
    #[bpaf(positional("DOC"))]
    doc: Option<String>,
}

impl Rfc {
    /// Execute the rfc command.
    fn exec(self) -> bool {
        if let Some(doc) = self.doc {
            if let Some(text) = rfc::render_doc(&doc) {
                print!("{text}");
                true
            } else {
                println!("{}", style(format!("unknown embedded RFC: {doc}")).red());
                false
            }
        } else {
            println!("{}", style("Embedded standards corpus").bold().cyan());
            print!("{}", rfc::render_doc_list());
            true
        }
    }
}

/// Generate a parsed but currently unimplemented subcommand wrapper.
macro_rules! stub_command {
    ($name:ident, $doc:literal) => {
        #[derive(Debug, Clone, Bpaf)]
        #[bpaf(command)]
        struct $name {
            #[bpaf(positional("PATH"))]
            path: Option<PathBuf>,
        }

        impl $name {
            fn exec_stub(
                self,
                command_name: &str,
            ) -> bool {
                ui::print_stub(command_name, self.path.as_deref(), $doc);
                false
            }
        }
    };
}

/// Generate a parsed but currently unimplemented library-aware subcommand wrapper.
macro_rules! stub_library_command {
    ($name:ident, $doc:literal) => {
        #[derive(Debug, Clone, Bpaf)]
        #[bpaf(command)]
        struct $name {
            #[bpaf(long)]
            library: bool,

            #[bpaf(positional("PATH"))]
            path: Option<PathBuf>,
        }

        impl $name {
            fn exec_stub(
                self,
                command_name: &str,
            ) -> bool {
                let _ = self.library;
                ui::print_stub(command_name, self.path.as_deref(), $doc);
                false
            }
        }
    };
}

stub_command!(Fmt, "Format and normalize source CDDL.");
stub_library_command!(Explain, "Explain schema behavior in human terms.");
stub_command!(Coverage, "Measure schema/vector coverage.");
stub_library_command!(Docs, "Generate schema documentation output.");
stub_command!(Lsp, "Start the CDDL language server.");

/// Render the effective CDDL schema the compiler actually reasons about:
/// expand named rules, constants, generics, sockets, plug choices, and
/// nested control operators into the readable wire shape used by lint
/// diagnostics for `.within` and `.and`.
#[derive(Debug, Clone, Bpaf)]
#[bpaf(command)]
#[allow(clippy::struct_excessive_bools)]
struct Render {
    /// Preserve library exports and named constants instead of folding them away.
    #[bpaf(long)]
    library: bool,

    /// Emit the effective CDDL as a JSON string for tools and scripts.
    #[bpaf(long)]
    json: bool,

    /// CDDL schema file to compile and render.
    #[bpaf(positional("PATH"))]
    path: PathBuf,
}

impl Render {
    /// Execute the render command.
    fn exec(self) -> bool {
        render::exec(&self.path, self.library, self.json)
    }
}

/// Validate CBOR against a compiled CDDL schema.
#[derive(Debug, Clone, Bpaf)]
#[bpaf(command)]
#[allow(clippy::struct_excessive_bools)]
struct Validate {
    /// Print compiler warnings in full instead of summarizing them.
    #[bpaf(long)]
    warn: bool,

    /// Always print the decoded CBOR tree, even on success.
    #[bpaf(long)]
    detailed: bool,

    /// Path to the CDDL schema file.
    #[bpaf(positional("SCHEMA"))]
    schema: PathBuf,

    /// Path to the CBOR input file, or standard input if omitted.
    #[bpaf(positional("PATH"))]
    path: Option<PathBuf>,
}

impl Validate {
    /// Execute the validate command.
    fn exec(
        self,
        force_no_color: bool,
    ) -> bool {
        validate::exec(
            &self.schema,
            self.path.as_deref(),
            self.warn,
            self.detailed,
            force_no_color,
        )
    }
}

/// Decode raw CBOR input into an EDN-like rendered tree.
#[derive(Debug, Clone, Bpaf)]
#[bpaf(command)]
struct Decode {
    /// Disable colorized output.
    #[bpaf(long)]
    no_color: bool,

    /// Pretty-print nested CBOR structures.
    #[bpaf(long)]
    pretty: bool,

    /// Path to CBOR input, or standard input if omitted.
    #[bpaf(positional("PATH"))]
    path: Option<PathBuf>,
}

impl Decode {
    /// Execute the decode command.
    fn exec(
        self,
        force_no_color: bool,
    ) -> bool {
        decode::exec(
            self.path.as_deref(),
            self.no_color || force_no_color,
            self.pretty,
        )
    }
}

/// Compile a single CDDL file and print the enriched AST tree dump.
fn compile_file_with_print(path: &PathBuf) -> bool {
    match CompiledCDDL::compile(path, None) {
        Ok(compiled) => {
            print_compiler_diagnostics(path, &compiled.warnings, false);
            println!("{compiled}");
            !has_error_diagnostics(&compiled.warnings)
        },
        Err(err) => {
            println!(
                "{} {}:\n{}",
                Emoji::new("🚨", "Compile Error"),
                path.display(),
                style(err).red()
            );
            false
        },
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{ColorMode, Command, OutputFormat, cli};

    #[test]
    fn parses_lint_and_compile_subcommands() {
        let lint = cli()
            .run_inner(&[
                "--color",
                "always",
                "lint",
                "--library",
                "--why",
                "input.cddl",
            ])
            .expect("lint command should parse");
        assert!(matches!(lint.color, ColorMode::Always));
        assert!(matches!(lint.format, OutputFormat::Rich));
        match lint.command {
            Command::Lint(args) => {
                assert!(args.library);
                assert!(args.why);
                assert!(!args.strict);
                assert_eq!(args.path, PathBuf::from("input.cddl"));
            },
            _ => panic!("expected lint command"),
        }

        let strict_lint = cli()
            .run_inner(&["lint", "--strict", "input.cddl"])
            .expect("strict lint command should parse");
        match strict_lint.command {
            Command::Lint(args) => {
                assert!(args.strict);
                assert_eq!(args.path, PathBuf::from("input.cddl"));
            },
            _ => panic!("expected lint command"),
        }

        let why = cli()
            .run_inner(&["why", "E016"])
            .expect("why command should parse");
        match why.command {
            Command::Why(args) => assert_eq!(args.code, vec!["E016".to_owned()]),
            _ => panic!("expected why command"),
        }

        let xref = cli()
            .run_inner(&["xref", ".cbor"])
            .expect("xref command should parse");
        match xref.command {
            Command::Xref(args) => assert_eq!(args.query, vec![".cbor".to_owned()]),
            _ => panic!("expected xref command"),
        }

        let rfc = cli()
            .run_inner(&["rfc", "rfc8610"])
            .expect("rfc command should parse");
        match rfc.command {
            Command::Rfc(args) => assert_eq!(args.doc, Some("rfc8610".to_owned())),
            _ => panic!("expected rfc command"),
        }

        let decode = cli()
            .run_inner(&[
                "--no-banner",
                "decode",
                "--no-color",
                "--pretty",
                "input.cbor",
            ])
            .expect("decode command should parse");
        match decode.command {
            Command::Decode(args) => {
                assert!(args.no_color);
                assert!(args.pretty);
                assert_eq!(args.path, Some(PathBuf::from("input.cbor")));
            },
            _ => panic!("expected decode command"),
        }
        assert!(decode.no_banner);

        let compile = cli()
            .run_inner(&["compile", "--library", "input.cddl"])
            .expect("compile command should parse");
        match compile.command {
            Command::Compile(args) => {
                assert!(args.library);
                assert_eq!(args.path, PathBuf::from("input.cddl"));
            },
            _ => panic!("expected compile command"),
        }

        let validate = cli()
            .run_inner(&["validate", "--detailed", "schema.cddl", "input.cbor"])
            .expect("validate command should parse");
        match validate.command {
            Command::Validate(args) => {
                assert!(args.detailed);
                assert!(!args.warn);
                assert_eq!(args.schema, PathBuf::from("schema.cddl"));
                assert_eq!(args.path, Some(PathBuf::from("input.cbor")));
            },
            _ => panic!("expected validate command"),
        }
    }
}
